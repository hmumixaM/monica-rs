import importlib
from pathlib import Path

import pytest


@pytest.fixture
def adapter(monkeypatch):
    monkeypatch.syspath_prepend(str(Path(__file__).parents[1]))
    return importlib.import_module('linkedin_messages')


def test_provider_group_flag_names_and_raw_message_survive_projection(adapter):
    raw = {'entityUrn':'message-one', 'sender':{'hostIdentityUrn':'me'},
           'body':{'text':'I promised tickets'}, 'deliveredAt':1794236700000,
           'renderContent':[{'attachment':{'reference':'provider-reference'}}]}
    conversation = adapter.source_conversation({'entityUrn':'group-one', 'groupChat':True,
        'conversationParticipants':[
            {'hostIdentityUrn':'me', 'participantType':{'member':{'firstName':{'text':'Me'}}}},
            {'hostIdentityUrn':'friend', 'participantType':{'member':{'firstName':{'text':'Alice'},'lastName':{'text':'Example'}}}}],
        'messages':{'elements':[raw]}},'me')
    row = adapter.records(conversation,'me')[0]
    assert row['is_group'] and row['direction'] == 'sent'
    assert row['participants'][0]['is_self'] and row['participants'][0]['role'] == 'sender'
    assert row['participants'][1]['name'] == 'Alice Example'
    assert row['raw_message']['renderContent'] == raw['renderContent']


def test_pagination_rejects_changed_schema_and_nonadvancing_cursor(adapter):
    with pytest.raises(RuntimeError): adapter.message_page({'errors':[{'message':'changed schema'}]})
    with pytest.raises(RuntimeError): adapter.next_cursor({'metadata':{'nextCursor':'same'}},'same')
    assert adapter.next_cursor({'metadata':{'nextCursor':'next'}},'prior') == 'next'
