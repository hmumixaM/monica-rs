#import <Foundation/Foundation.h>
#import <sqlite3.h>

static NSString *stringColumn(sqlite3_stmt *statement, int index) {
    const unsigned char *value = sqlite3_column_text(statement, index);
    return value ? @((const char *)value) : @"";
}
static NSArray *messageParticipants(sqlite3 *db, long long chat) {
    NSMutableArray *people = [NSMutableArray array]; sqlite3_stmt *query = NULL;
    if (sqlite3_prepare_v2(db, "SELECT h.id FROM handle h JOIN chat_handle_join j ON j.handle_id=h.ROWID WHERE j.chat_id=?", -1, &query, NULL) == SQLITE_OK) {
        sqlite3_bind_int64(query, 1, chat);
        while (sqlite3_step(query) == SQLITE_ROW) {
            NSString *identifier = stringColumn(query, 0);
            [people addObject:@{@"id":identifier,@"role":@"participant",@"emails":[identifier containsString:@"@"] ? @[identifier] : @[],@"phones":[identifier hasPrefix:@"+"] ? @[identifier] : @[]}];
        }
    }
    sqlite3_finalize(query); return people;
}
static NSArray *messageAttachments(sqlite3 *db, long long message) {
    NSMutableArray *attachments = [NSMutableArray array]; sqlite3_stmt *query = NULL;
    if (sqlite3_prepare_v2(db, "SELECT a.guid,a.filename,a.mime_type,a.transfer_name FROM attachment a JOIN message_attachment_join j ON j.attachment_id=a.ROWID WHERE j.message_id=?", -1, &query, NULL) == SQLITE_OK) {
        sqlite3_bind_int64(query, 1, message);
        while (sqlite3_step(query) == SQLITE_ROW) [attachments addObject:@{@"id":stringColumn(query,0),@"path":stringColumn(query,1),@"content_type":stringColumn(query,2),@"filename":stringColumn(query,3)}];
    }
    sqlite3_finalize(query); return attachments;
}
char *monica_messages_read_path(const char *databasePath, const char *previousCursor) {
    @autoreleasepool {
        NSMutableDictionary *cursor = previousCursor && previousCursor[0] ? [[NSJSONSerialization JSONObjectWithData:[@(previousCursor) dataUsingEncoding:NSUTF8StringEncoding] options:NSJSONReadingMutableContainers error:nil] mutableCopy] : [NSMutableDictionary dictionary];
        if (!cursor) cursor = [NSMutableDictionary dictionary];
        NSString *path = @(databasePath);
        sqlite3 *db = NULL; NSDictionary *output; NSMutableArray *records = [NSMutableArray array];
        if (sqlite3_open_v2(path.UTF8String, &db, SQLITE_OPEN_READONLY, NULL) != SQLITE_OK) {
            output = @{@"error":@"无法读取这台 Mac 的 Messages 数据库。请在系统设置的完全磁盘访问权限中允许 Monica，再重试。"};
        } else {
            sqlite3_busy_timeout(db, 3000);
            NSMutableSet *columns = [NSMutableSet set]; sqlite3_stmt *query = NULL;
            sqlite3_prepare_v2(db, "PRAGMA table_info(message)", -1, &query, NULL);
            while (sqlite3_step(query) == SQLITE_ROW) [columns addObject:stringColumn(query, 1)];
            sqlite3_finalize(query); query = NULL;
            long long turn = [cursor[@"turn"] longLongValue];
            NSString *phase = @[@"live",@"recent",@"edited",@"live",@"history",@"retracted"][turn % 6];
            if ([phase isEqual:@"history"] && ![cursor[@"recent_complete"] boolValue]) phase = @"recent";
            if ([cursor[[phase stringByAppendingString:@"_complete"]] boolValue]) phase = @"live";
            NSString *field = [phase isEqual:@"edited"] ? @"date_edited" : [phase isEqual:@"retracted"] ? @"date_retracted" : @"ROWID";
            BOOL descending = [phase isEqual:@"recent"] || [phase isEqual:@"history"] || ([phase isEqual:@"live"] && !cursor[@"live"]);
            long long after = [cursor[phase] longLongValue];
            long long cutoff = cursor[@"cutoff"] ? [cursor[@"cutoff"] longLongValue] : (long long)((NSDate.date.timeIntervalSinceReferenceDate - 90*86400) * 1e9);
            cursor[@"cutoff"] = @(cutoff);
            NSString *edited = [columns containsObject:@"date_edited"] ? @"m.date_edited" : @"0";
            NSString *retracted = [columns containsObject:@"date_retracted"] ? @"m.date_retracted" : @"0";
            BOOL supported = [field isEqual:@"ROWID"] || [columns containsObject:field];
            NSString *range = descending ? (after ? [NSString stringWithFormat:@"m.ROWID<%lld",after] : @"1") : [NSString stringWithFormat:@"m.%@>%lld",field,after];
            if (![field isEqual:@"ROWID"]) range = [NSString stringWithFormat:@"m.%@>0 AND (m.%@>%lld OR (m.%@=%lld AND m.ROWID>%lld))",field,field,after,field,after,[cursor[[phase stringByAppendingString:@"_rowid"]] longLongValue]];
            if ([phase isEqual:@"recent"]) range = [range stringByAppendingFormat:@" AND m.date>=%lld",cutoff];
            if ([phase isEqual:@"history"]) range = [range stringByAppendingFormat:@" AND m.date<%lld",cutoff];
            NSString *sql = [NSString stringWithFormat:@"SELECT m.ROWID,m.guid,m.text,m.attributedBody,m.date,m.is_from_me,h.id,(SELECT chat_id FROM chat_message_join WHERE message_id=m.ROWID LIMIT 1),%@,%@,m.%@ FROM message m LEFT JOIN handle h ON h.ROWID=m.handle_id WHERE %@ ORDER BY m.%@ %@,m.ROWID LIMIT 100",edited,retracted,field,range,field,descending ? @"DESC" : @"ASC"];
            int result = supported ? sqlite3_prepare_v2(db, sql.UTF8String, -1, &query, NULL) : SQLITE_OK;
            long long watermark = after;
            NSISO8601DateFormatter *format = [[NSISO8601DateFormatter alloc] init];
            if (result == SQLITE_OK && supported) {
                while ((result = sqlite3_step(query)) == SQLITE_ROW) {
                    long long row = sqlite3_column_int64(query,0), chat = sqlite3_column_int64(query,7);
                    NSString *body = stringColumn(query,2); BOOL decodeError = NO; NSString *rawAttributed = @"";
                    if (!body.length && sqlite3_column_bytes(query,3)) {
                        NSData *blob = [NSData dataWithBytes:sqlite3_column_blob(query,3) length:sqlite3_column_bytes(query,3)];
                        @try {
                            id value = [NSUnarchiver unarchiveObjectWithData:blob];
                            if ([value isKindOfClass:NSAttributedString.class]) body = [value string]; else decodeError = YES;
                        } @catch (NSException *exception) { decodeError = YES; }
                        if (decodeError) rawAttributed = [blob base64EncodedStringWithOptions:0];
                    }
                    BOOL own = sqlite3_column_int(query,5) != 0;
                    NSString *sender = own ? @"self" : stringColumn(query,6);
                    NSMutableArray *participants = [NSMutableArray array];
                    for (NSDictionary *person in messageParticipants(db, chat)) [participants addObject:[person mutableCopy]];
                    BOOL found = NO;
                    for (NSMutableDictionary *person in participants) if ([person[@"id"] isEqual:sender]) { person[@"role"]=@"sender"; found=YES; }
                    if (!found) [participants addObject:@{@"id":sender,@"role":@"sender",@"is_self":@(own),@"emails":[sender containsString:@"@"] ? @[sender] : @[],@"phones":[sender hasPrefix:@"+"] ? @[sender] : @[]}];
                    double stamp = sqlite3_column_double(query,4); if (fabs(stamp)>1e12) stamp /= 1e9;
                    [records addObject:@{@"external_id":stringColumn(query,1),@"thread_id":[NSString stringWithFormat:@"chat:%lld",chat],@"kind":@"message",@"body":body,
                        @"occurred_at":[format stringFromDate:[NSDate dateWithTimeIntervalSinceReferenceDate:stamp]],@"participants":participants,
                        @"direction":own ? @"sent" : @"received",@"attachments":messageAttachments(db,row),@"text_decode_error":@(decodeError),@"raw_attributed_body":rawAttributed,
                        @"edited_at":@(sqlite3_column_int64(query,8)),@"deleted":(sqlite3_column_int64(query,9)>0 ? @YES : @NO),@"included":@YES}];
                    long long current = sqlite3_column_int64(query,10);
                    watermark = descending && ![phase isEqual:@"live"] ? current : MAX(watermark,current);
                    cursor[[phase stringByAppendingString:@"_rowid"]] = @(row);
                }
            }
            sqlite3_finalize(query);
            if (result != SQLITE_DONE && result != SQLITE_OK) output = @{@"error":@"Messages 数据库读取失败，未推进同步位置。"};
            else {
                cursor[phase]=@(watermark); cursor[@"turn"]=@(turn+1);
                if (([phase isEqual:@"recent"] || [phase isEqual:@"history"]) && records.count<100) cursor[[phase stringByAppendingString:@"_complete"]]=@YES;
                output = @{@"records":records,@"cursor":cursor,@"coverage":@"这台 Mac 实际同步并可读取的 Messages 历史"};
            }
        }
        if (db) sqlite3_close(db);
        NSData *json = [NSJSONSerialization dataWithJSONObject:output options:0 error:nil];
        return strdup([[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding].UTF8String);
    }
}

char *monica_messages_read(const char *previousCursor, int unused) {
    @autoreleasepool {
        NSString *path = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Messages/chat.db"];
        return monica_messages_read_path(path.UTF8String, previousCursor);
    }
}
