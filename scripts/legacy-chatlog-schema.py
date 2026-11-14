"""Explicit disposition for the pinned WCDB mirror format, without account IDs.

Unknown database paths or tables require review before a migration can complete.
Operational authentication caches remain only in the sealed offline snapshot;
none of their values are installed in the application's queryable database.
"""
import re

HISTORY={
 'bizchat/bizchat.db':'name2id chat_group user_info my_user_info',
 'contact/contact.db':'contact name2id chat_room chat_room_info_detail stranger chatroom_member biz_info openim_appid openim_acct_type openim_wording contact_label room_verify_application oplog',
 'emoticon/emoticon.db':'kNonStoreEmoticonTable kFavEmoticonOrderTable kCustomEmoticonOrderTable kExpressRecentUseEemoticonTable kStoreEmoticonPackageTable kStoreEmoticonFilesTable kStoreEmoticonCaptionsTable',
 'favorite/favorite.db':'fav_db_item fav_tag_db_item fav_bind_tag_db_item Name2Id',
 'general/general.db':'revokemessage wcfinderlivestatus wcfinderuserpage FMessageTable new_tips ilink_voip biz_pay_status biz_subscribe_status transferTable redEnvelopeTable websearch_record teenager_apply_access_agree_info reddot reddot_record brand_search_record SearchRecent ForwardRecent handoff_remind_v0 groupPayTable revokebatchmessage wacontact WeAppBizAttrSyncBufferTableV02 reddot_last_notify',
 'hardlink/hardlink.db':'image_hardlink_info_v4 file_hardlink_info_v4 video_hardlink_info_v4 dir2id',
 'head_image/head_image.db':'head_image',
 'message/message_resource.db':'ChatName2Id SenderName2Id MessageResourceInfo MessageResourceDetail FtsDeleteInfo',
 'migrate/unspportmsg.db':'UnsupportMessage',
 'session/session.db':'SessionTable SessionDeleteTable Name2Id SessionNoContactInfoTable SessionDraft',
 'sns/sns.db':'SnsMessage_tmp3 SnsIgnoredDataItem SnsTopItem_1 SnsErrorMessage SnsNoteVoice SnsDraft SnsPublishTask SnsAdTimeLine SnsPendingDraftDeletionTable',
}
DERIVED={
 'contact/contact.db':'encrypt_name2id ticket_info stranger_ticket_info',
 'favorite/favorite.db':'config buff resync_task sync_client_storage',
 'hardlink/hardlink.db':'db_info file_checkpoint_v4 video_checkpoint_v4 talker_checkpoint_v4',
 'message/message_resource.db':'FtsRange',
 'session/session.db':'SessionUnreadListTable_1 SessionUnreadStatTable_1',
 'sns/sns.db':'SnsMainTimeLineBreakFlag SnsUserTimeLineBreakFlagV2',
}


def classify(file,table):
    message=bool(re.fullmatch(r'message/(?:biz_)?message_[0-9]+\.db',file))
    voice=bool(re.fullmatch(r'message/media_[0-9]+\.db',file))
    known_file=file in HISTORY or file in DERIVED or message or voice or file in {'message/weclaw.db','solitaire/solitaire.db'}
    if not known_file:raise ValueError('unclassified Chatlog database path')
    if table=='sqlite_sequence':return 'derived','SQLite allocator; rebuilt by the target database'
    if message:
        if re.fullmatch(r'Msg_[0-9a-fA-F]{32}',table):return 'business','messages and immutable evidence reconciled through the bounded Reader'
        if table in {'TimeStamp','Name2Id','wcdb_builtin_compression_record'}:return 'derived','snapshot mapping and compression metadata; not executable target work'
        if table in {'DeleteInfo','DeleteResInfo','SendInfo','HistorySysMsgInfo','HistoryAddMsgInfo','MessageGroupTimeInfo'}:return 'history','source delivery/deletion/history evidence; no guessed deletion target'
    if voice:
        if table=='VoiceInfo':return 'history','original voice blobs with exact chat and message provenance'
        if table in {'TimeStamp','Name2Id'}:return 'derived','voice identity lookup retained in the sealed snapshot'
    if file=='sns/sns.db' and table=='SnsTimeLine':return 'business','Moments, interactions and immutable source versions'
    if file=='solitaire/solitaire.db' and re.fullmatch(r'Solitaire(?:Valid|Fold)?_[0-9a-fA-F]{32}',table):return 'history','group collection history'
    if table in HISTORY.get(file,'').split():return 'history','pageable original source rows; supplementary account/group projection where supported'
    if table in DERIVED.get(file,'').split():return 'derived','operational state retained in the sealed snapshot; authentication and executable work are not installed'
    raise ValueError('unclassified Chatlog source table')
