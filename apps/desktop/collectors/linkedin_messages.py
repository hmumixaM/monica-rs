"""Independent message lane, with durable pages and per-conversation history cursors."""
import argparse
import asyncio
import hashlib
import json
import re
import signal
import sys
import time
from pathlib import Path
from urllib.parse import quote, unquote

from publishing import atomic_json

sys.path.insert(0, str(Path(__file__).parent / 'vendor'))
from linkedin_enrichment.browser import PersistentChromium, assert_access
from linkedin_enrichment.messaging import (
    CONVERSATIONS_QUERY_ID,
    MESSAGES_QUERY_ID,
    MESSAGING_URL,
    _root_with_elements,
    parse_conversation,
    parse_message,
)


def message_page(payload):
    root = _root_with_elements(payload)
    if payload.get('errors') or not isinstance(root.get('elements'), list):
        raise RuntimeError('LinkedIn message schema changed; cursor was not advanced')
    return root


def next_cursor(payload, previous=None):
    cursor = payload.get('metadata', {}).get('nextCursor') or payload.get('nextCursor')
    if cursor is not None and not isinstance(cursor, str): raise RuntimeError('Invalid LinkedIn page cursor')
    if cursor and cursor == previous: raise RuntimeError('LinkedIn page cursor did not advance')
    return cursor


def source_message(row, self_urn):
    value = parse_message(row, self_urn=self_urn)
    return {**value, 'raw_message': row} if value else None


def source_conversation(row, self_urn):
    value = parse_conversation(row, self_urn=self_urn)
    if not value: return None
    names = {}
    for participant in row.get('conversationParticipants', []):
        member = (participant.get('participantType') or {}).get('member') or {}
        def label(field, member=member):
            item = member.get(field) or ''
            return item.get('text', '') if isinstance(item, dict) else item
        names[participant.get('hostIdentityUrn')] = ' '.join(filter(None, [label('firstName'), label('lastName')]))
    return {**value, 'is_group': bool(row.get('groupChat')), 'participant_names': names,
        'messages': [message for item in (row.get('messages') or {}).get('elements', [])
                     if (message := source_message(item, self_urn))]}


def records(conversation, self_urn):
    result = []
    for message in conversation.get('messages', []):
        sender = message.get('sender_urn')
        participants = list(dict.fromkeys([*conversation.get('participants', []), *([sender] if sender else [])]))
        result.append({'external_id': message['message_urn'], 'thread_id': conversation['conversation_urn'],
            'kind': 'message', 'body': message.get('body') or '', 'title': message.get('subject') or conversation.get('title') or '',
            'occurred_at': message.get('delivered_at'), 'included': True, 'is_group': conversation.get('is_group', False),
            'raw_message': message.get('raw_message', {}),
            'direction': 'sent' if sender == self_urn else 'received',
            'participants': [{'id': person, 'name': conversation.get('participant_names', {}).get(person) or '', 'is_self': person == self_urn, 'role': 'sender' if person == sender else 'participant'} for person in participants]})
    return result


def publish(folder, rows):
    if rows:
        atomic_json(folder / 'pending' / f'{time.time_ns():020d}.json', {'records': rows})


async def cycle(root, raw):
    auth = root / 'messages-auth.json'
    if not auth.exists(): raise RuntimeError('LinkedIn login required')
    async with PersistentChromium(root / 'messages-browser') as browser:
        await browser.import_storage_state(auth)
        page = browser.page
        async with page.expect_response(lambda response: 'messengerConversations' in response.url, timeout=30000) as capture:
            await page.goto(MESSAGING_URL, wait_until='domcontentloaded')
        response = await capture.value
        await assert_access(page, expected_selector='main')
        matched = re.search(r'mailboxUrn:([^,)]+)', unquote(response.url))
        if not matched: raise RuntimeError('LinkedIn mailbox identity missing')
        self_urn = matched.group(1)
        key = hashlib.sha256(self_urn.encode()).hexdigest()
        folder = raw / 'messages' / key
        folder.mkdir(parents=True, exist_ok=True, mode=0o700)
        atomic_json(folder / 'account.json', {'account': self_urn})
        state_path = folder / 'state.json'
        state = json.loads(state_path.read_text()) if state_path.exists() else {'conversations': {}}
        token = next((item['value'].strip('"') for item in await browser.context.cookies() if item['name'] == 'JSESSIONID'), None)
        if not token: raise RuntimeError('LinkedIn CSRF token missing')

        async def fetch(query, variables):
            url = 'https://www.linkedin.com/voyager/api/voyagerMessagingGraphQL/graphql?queryId=' + query + '&variables=' + variables
            result = await page.evaluate('''async ({url, token}) => {
              const r = await fetch(url, {credentials:'include', headers:{'csrf-token':token,'x-restli-protocol-version':'2.0.0'}});
              return {status:r.status, body:await r.text()};
            }''', {'url': url, 'token': token})
            if result['status'] != 200: raise RuntimeError(f"LinkedIn message HTTP {result['status']}")
            return message_page(json.loads(result['body']))

        def accept_conversations(payload):
            for row in payload.get('elements', []):
                conversation = source_conversation(row, self_urn)
                if conversation:
                    urn = conversation['conversation_urn']
                    previous = state['conversations'].get(urn, {})
                    state['conversations'][urn] = {**previous, 'data': conversation}
                    publish(folder, records(conversation, self_urn))

        accept_conversations(message_page(await response.json()))
        if not state.get('conversations_complete'):
            cursor = 'nextCursor:' + quote(state['next_conversation'], safe='') if state.get('next_conversation') else f'lastUpdatedBefore:{int(time.time()*1000)}'
            payload = await fetch(CONVERSATIONS_QUERY_ID, '(query:(predicateUnions:List((conversationCategoryPredicate:(category:PRIMARY_INBOX)))),count:20,mailboxUrn:' + quote(self_urn, safe='') + ',' + cursor + ')')
            accept_conversations(payload)
            state['next_conversation'] = next_cursor(payload, state.get('next_conversation'))
            state['conversations_complete'] = not bool(state['next_conversation'])
        # Persist each page before advancing a cursor; history never blocks new previews.
        atomic_json(state_path, state)
        conversations = sorted(state['conversations'].values(), key=lambda item: item.get('checked_at', 0))[:5]
        for entry in conversations:
            conversation = entry['data']
            cursor = entry.get('next_message')
            suffix = ',nextCursor:' + quote(cursor, safe='') if cursor else ''
            payload = await fetch(MESSAGES_QUERY_ID, '(conversationUrn:' + quote(conversation['conversation_urn'], safe='') + suffix + ')')
            parsed = [source_message(row, self_urn) for row in payload.get('elements', [])]
            publish(folder, records({**conversation, 'messages': [item for item in parsed if item]}, self_urn))
            entry['next_message'] = next_cursor(payload, cursor)
            entry['messages_complete'] = not bool(entry['next_message'])
            entry['checked_at'] = time.time()
            atomic_json(state_path, state)
        atomic_json(root / 'messages-status.json', {'state': 'connected', 'account': self_urn,
                    'conversations': len(state['conversations']), 'coverage': '已采集页面；平台未返回的历史不计为完整覆盖'})
        return not state.get('conversations_complete') or any(not entry.get('messages_complete') for entry in state['conversations'].values())


async def run(data_root):
    stopped = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT): loop.add_signal_handler(sig, stopped.set)
    root = data_root / 'collectors/linkedin'
    while not stopped.is_set():
        task = asyncio.create_task(cycle(root, data_root / 'raw/linkedin'))
        stop = asyncio.create_task(stopped.wait())
        done, _ = await asyncio.wait((task, stop), return_when=asyncio.FIRST_COMPLETED)
        if stop in done:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            break
        stop.cancel()
        backfill = False
        try: backfill = task.result()
        except Exception as exc:  # noqa: BLE001 -- isolate this connector and expose the failure state
            atomic_json(root / 'messages-status.json', {'state': 'error', 'error': type(exc).__name__})
        try: await asyncio.wait_for(stopped.wait(), timeout=1 if backfill else 60)
        except TimeoutError: pass


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--data-root', type=Path, required=True)
    asyncio.run(run(parser.parse_args().data_root))
