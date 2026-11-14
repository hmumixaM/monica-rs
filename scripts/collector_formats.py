"""Pure archive normalization shared by the Rust collector bridge.

Keeps the original Monica Next post, Story and media-slot contracts.
No collector, credentials, filesystem publication or network calls.
"""
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path
UTC = timezone.utc


def user_identity(value):
    if not isinstance(value,dict):return None
    name=value.get('username')
    if not isinstance(name,str) or not re.fullmatch(r'[A-Za-z0-9_.]{1,30}',name):return None
    return {'username':name,'external_id':str(value.get('id') or value.get('pk') or value.get('userid') or ''),'name':value.get('full_name') or name,'url':f'https://www.instagram.com/{name}/'}


def edges(value):
    if not isinstance(value,dict):return []
    return [item.get('node',{}) for item in value.get('edges') or [] if isinstance(item,dict)]


def timestamp(value):
    if value is None:return None
    try:
        if isinstance(value,(int,float)) or (isinstance(value,str) and value.isdigit()):return datetime.fromtimestamp(float(value),UTC).isoformat()
        return datetime.fromisoformat(str(value).replace("Z", "+00:00")).isoformat()
    except (ValueError,TypeError,OverflowError,OSError):return None


def normalize_post(node, relationships=None):
    relationships=relationships or {}
    phone=node.get('iphone_struct') or {}
    code=node.get('shortcode') or node.get('code') or phone.get('code')
    code=code if isinstance(code,str) and re.fullmatch(r'[A-Za-z0-9_-]+',code) else None
    caption=node.get('caption')
    if isinstance(caption,dict):caption=caption.get('text')
    if not isinstance(caption,str):caption=next((x.get('text') for x in edges(node.get('edge_media_to_caption')) if isinstance(x.get('text'),str)), None)
    if caption is None:
        phone_caption=phone.get('caption') or {}
        caption=phone_caption.get('text','') if isinstance(phone_caption,dict) else str(phone_caption)
    graphql_children=edges(node.get('edge_sidecar_to_children'))
    phone_children=phone.get('carousel_media') or node.get('carousel_media') or []
    if graphql_children and len(graphql_children)==len(phone_children):
        raw_media=[{**mobile,**graphql} for mobile,graphql in zip(phone_children,graphql_children)]
    else:
        raw_media=graphql_children or phone_children or [{**phone,**node}]
    media=[];tagged=[]
    for index,child in enumerate(raw_media):
        video=child.get('is_video') or child.get('media_type')==2
        video_url=child.get('video_url') or next((v.get('url') for v in child.get('video_versions') or [] if v.get('url')),None)
        images=child.get('display_resources') or (child.get('image_versions2') or {}).get('candidates') or []
        image_url=child.get('display_url') or child.get('thumbnail_src') or next((v.get('src') or v.get('url') for v in images),None)
        dimensions=child.get('dimensions') or {}
        duration=child.get('video_duration')
        media.append({'external_id':str(child.get('id') or child.get('pk') or ''),'source_url':video_url or image_url,'thumbnail_url':image_url,
            'media_type':'video' if video or video_url else 'image','width':dimensions.get('width') or child.get('original_width'),'height':dimensions.get('height') or child.get('original_height'),
            'duration_ms':round(duration*1000) if isinstance(duration,(int,float)) else None,'accessibility_caption':child.get('accessibility_caption'),
            'image_resources':[{'url':v.get('src') or v.get('url'),'width':v.get('config_width') or v.get('width'),'height':v.get('config_height') or v.get('height')} for v in images if v.get('src') or v.get('url')],
            'video_resources':[{'url':v.get('src') or v.get('url'),'width':v.get('config_width') or v.get('width'),'height':v.get('config_height') or v.get('height')} for v in child.get('video_resources') or child.get('video_versions') or [] if v.get('src') or v.get('url')]})
        tagged_rows=edges(child.get('edge_media_to_tagged_user'))+((child.get('usertags') or {}).get('in') or [])
        for entry in tagged_rows:
            user=user_identity(entry.get('user'))
            if user:tagged.append({**user,'media_index':index+1,'x':entry.get('x'),'y':entry.get('y')})
    # GraphQL also exposes tags on the carousel parent.
    for entry in edges(node.get('edge_media_to_tagged_user'))+((phone.get('usertags') or {}).get('in') or []):
        user=user_identity(entry.get('user'))
        if user and not any(x['username']==user['username'] for x in tagged):tagged.append(user)
    comments=node.get('edge_media_to_parent_comment') or node.get('edge_media_to_comment') or {}
    interactions=[];previews={'commenters':[],'repliers':[],'likers':[]}
    def add_comment(comment,parent=None):
        author=user_identity(comment.get('owner') or comment.get('user'))
        if not author:return
        group='repliers' if parent else 'commenters'
        if len(previews[group])<5 and not any(x['username']==author['username'] for x in previews[group]):previews[group].append(author)
        cid=str(comment.get('id') or comment.get('pk') or hashlib.sha256(json.dumps(comment,sort_keys=True).encode()).hexdigest())
        interactions.append({'external_id':cid,'interaction_type':'reply' if parent else 'comment','author_external_id':author['external_id'] or 'username:'+author['username'],'author_name':author['username'],'content':{'text':comment.get('text'),'parent_comment_id':parent},'occurred_at':timestamp(comment.get('created_at') or comment.get('created_at_utc'))})
        for reply in edges(comment.get('edge_threaded_comments'))+list(comment.get('preview_child_comments') or [])+list(comment.get('answers') or []):
            if not parent:add_comment(reply,cid)
    for comment in edges(comments)+list(phone.get('preview_comments') or [])+list((relationships.get('comments') or {}).get('records') or []):add_comment(comment)
    like_data=node.get('edge_media_preview_like') or node.get('edge_liked_by') or {}
    for person in edges(like_data)+list(phone.get('facepile_top_likers') or [])+list((relationships.get('likes') or {}).get('profiles') or []):
        user=user_identity(person)
        if user and len(previews['likers'])<5 and not any(x['username']==user['username'] for x in previews['likers']):previews['likers'].append(user)
    mentions=list(dict.fromkeys(re.findall(r'(?<![\w@])@([A-Za-z0-9_](?:[A-Za-z0-9_.]{0,28}[A-Za-z0-9_])?)',caption)))
    for entry in relationships.get('caption_mentions') or []:
        name=entry if isinstance(entry,str) else entry.get('username') if isinstance(entry,dict) else None
        if isinstance(name,str) and re.fullmatch(r'[A-Za-z0-9_.]{1,30}',name) and name not in mentions:mentions.append(name)
    for entry in relationships.get('tagged_users') or []:
        user=user_identity({'username':entry} if isinstance(entry,str) else entry)
        if user and not any(t['username']==user['username'] for t in tagged):tagged.append(user)
    location=node.get('location') or phone.get('location')
    if isinstance(location,dict):location={k:v for k,v in location.items() if v is not None and k!='profile_pic_url'}
    else:location=None
    return {'post_id':str(node.get('id') or node.get('pk') or ''),'shortcode':code,'permalink':f'https://www.instagram.com/p/{code}/' if code else None,
        'published_at':timestamp(node.get('date') or node.get('taken_at_timestamp') or node.get('taken_at') or phone.get('taken_at')),
        'text':caption,'media_type':'carousel' if len(raw_media)>1 or node.get('__typename')=='GraphSidecar' else ('video' if media and media[0]['media_type']=='video' else 'image'),
        'media':media,'location':location,'tagged_users':tagged,'caption_mentions':[{'username':name,'url':f'https://www.instagram.com/{name}/'} for name in mentions],
        'like_count':like_data.get('count',node.get('likes') if isinstance(node.get('likes'),int) else phone.get('like_count')),
        'comment_count':comments.get('count',node.get('comments') if isinstance(node.get('comments'),int) else phone.get('comment_count')),
        'interactions':list({(i['interaction_type'],i['external_id']):i for i in interactions}.values()),'people_preview':previews,'people_preview_is_complete':False,
        'interaction_collection':{key:{'policy':(relationships.get(key) or {}).get('policy'),'count':(relationships.get(key) or {}).get('count')} for key in ('comments','likes')}}



def story_locations(raw):
    """Keep explicit place tags; never treat sticker screen x/y as GPS."""
    locations = []
    seen = set()
    for container in (raw, raw.get('iphone_struct') or {}):
        candidates = [container.get('location')]
        candidates += [item.get('location') for item in container.get('story_locations', []) if isinstance(item, dict)]
        for value in candidates:
            if not isinstance(value, dict):
                continue
            place = {k: v for k, v in value.items() if v is not None and k != 'profile_pic_url'}
            lat, lng = place.get('lat', place.get('latitude')), place.get('lng', place.get('longitude'))
            try:
                lat, lng = float(lat), float(lng)
                if -90 <= lat <= 90 and -180 <= lng <= 180:
                    place['latitude'], place['longitude'] = lat, lng
            except (ValueError, TypeError):
                pass
            place['coordinate_source'] = 'instagram_place_tag'
            key = str(place.get('pk') or place.get('id') or hashlib.sha256(json.dumps(place, sort_keys=True, separators=(",", ":")).encode()).hexdigest())
            if key not in seen:
                locations.append(place)
                seen.add(key)
    return locations


def instagram_file_slots(metadata, node, files=None, normalized=None):
    """Map archive filenames to explicit carousel positions, including sparse downloads."""
    import re

    children = (normalized if normalized is not None else normalize_post(node))['media']
    for path in sorted(files if files is not None else metadata.parent.glob(metadata.stem + "*")):
        if path.suffix.lower() not in (".jpg", ".jpeg", ".png", ".webp", ".mp4"):
            continue
        suffix = path.stem[len(metadata.stem) :]
        number = re.fullmatch(r"_(\d+)", suffix)
        position = int(number.group(1)) if number else 1 if not suffix and len(children) == 1 else 0
        if not 1 <= position <= len(children):
            yield path, None, None
            continue
        child = children[position - 1]
        video = path.suffix.lower() == ".mp4"
        url = child.get("source_url") if video else child.get("thumbnail_url")
        role = f"{'thumbnail' if child.get('media_type')=='video' and not video else 'media'}-{position}"
        yield path, role, url


def linkedin_media(post):
    """Normalize LinkedIn's image URL list into the common timeline shape."""
    rows = []
    urls = post.get("image_urls") or post.get("images") or []
    if isinstance(urls, str):
        urls = [urls]
    if not isinstance(urls, list):
        urls = []
    for index, url in enumerate(urls):
        if isinstance(url, dict):
            url = url.get("url") or url.get("source_url") or url.get("image_url")
        if not isinstance(url, str) or not url.strip():
            continue
        rows.append(
            {
                "role": f"media-{index + 1}",
                "media_type": "image",
                "source_url": url.strip(),
                "thumbnail_url": url.strip(),
            }
        )
    video = post.get("video_url")
    if isinstance(video, str) and video.strip():
        poster = rows[0]['source_url'] if rows else None
        rows = [{'role': 'video-1', 'media_type': 'video', 'source_url': video.strip(), 'thumbnail_url': poster}] + rows[1:]
    return rows


def normalize_story(doc):
    external = str(doc['mediaid'])
    raw = doc.get('raw_metadata') or {}
    video = bool(doc.get('is_video'))
    video_urls = raw.get('video_resources') or (raw.get('iphone_struct') or {}).get('video_versions') or []
    video_url = raw.get('video_url') or next((r.get('src') or r.get('url') for r in video_urls if r.get('src') or r.get('url')), None)
    image_url = raw.get('display_url') or f'archive://instagram/{external}/image'
    username = doc.get('owner_username')
    content = dict(text=doc.get('caption') or '', username=username,
                   owner=dict(id=str(doc['owner_id']), username=username),
                   expires_at=doc.get('expiring_at'), coverage_status='archived',
                   media=[dict(media_type='video' if video else 'image',
                               source_url=(video_url or f'archive://instagram/{external}/video') if video else image_url,
                               thumbnail_url=image_url)],
                   permalink=f'https://www.instagram.com/stories/{username}/{external}/' if username else None,
                   archive_manifest=doc)
    locations = story_locations(raw)
    if locations:
        content.update(locations=locations, location=locations[0])
    return content
