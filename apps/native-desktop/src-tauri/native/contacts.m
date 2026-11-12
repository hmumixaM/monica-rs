#import <Contacts/Contacts.h>
#import <Foundation/Foundation.h>
#include <stdatomic.h>

static atomic_uint_fast64_t contactsRevision = 1;
uint64_t monica_contacts_revision(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        [NSNotificationCenter.defaultCenter addObserverForName:CNContactStoreDidChangeNotification
            object:nil queue:nil usingBlock:^(NSNotification *note) { atomic_fetch_add(&contactsRevision, 1); }];
    });
    return atomic_load(&contactsRevision);
}

@interface MonicaContactsVisitor : NSObject <CNChangeHistoryEventVisitor>
@property NSMutableArray *records;
@property BOOL reset;
@end

static NSDictionary *contactRecord(CNContact *contact) {
    NSMutableArray *emails = [NSMutableArray array], *phones = [NSMutableArray array];
    for (CNLabeledValue *value in contact.emailAddresses) [emails addObject:value.value];
    for (CNLabeledValue<CNPhoneNumber *> *value in contact.phoneNumbers) [phones addObject:value.value.stringValue];
    NSString *name = [CNContactFormatter stringFromContact:contact style:CNContactFormatterStyleFullName] ?: contact.organizationName;
    NSMutableDictionary *value = [@{ @"external_id":contact.identifier, @"kind":@"contact", @"title":name ?: @"",
        @"body":contact.organizationName ?: @"", @"included":@YES, @"emails":emails, @"phones":phones,
        @"organization":contact.organizationName ?: @"", @"job_title":contact.jobTitle ?: @"",
        @"participants":@[@{@"id":contact.identifier,@"name":name ?: @"",@"emails":emails,@"phones":phones,@"role":@"subject"}] } mutableCopy];
    if (contact.birthday) {
        NSMutableDictionary *birthday = [NSMutableDictionary dictionary];
        if (contact.birthday.year != NSDateComponentUndefined) birthday[@"year"] = @(contact.birthday.year);
        if (contact.birthday.month != NSDateComponentUndefined) birthday[@"month"] = @(contact.birthday.month);
        if (contact.birthday.day != NSDateComponentUndefined) birthday[@"day"] = @(contact.birthday.day);
        value[@"birthday"] = birthday;
    }
    return value;
}

@implementation MonicaContactsVisitor
- (instancetype)init { if ((self = [super init])) self.records = [NSMutableArray array]; return self; }
- (void)visitDropEverythingEvent:(CNChangeHistoryDropEverythingEvent *)event { self.reset = YES; }
- (void)visitAddContactEvent:(CNChangeHistoryAddContactEvent *)event { [self.records addObject:contactRecord(event.contact)]; }
- (void)visitUpdateContactEvent:(CNChangeHistoryUpdateContactEvent *)event { [self.records addObject:contactRecord(event.contact)]; }
- (void)visitDeleteContactEvent:(CNChangeHistoryDeleteContactEvent *)event { [self.records addObject:@{@"external_id":event.contactIdentifier,@"kind":@"contact",@"deleted":@YES}]; }
@end

// Called on a Rust blocking thread inside the signed main app. TCC belongs to Monica.
char *monica_contacts_read(const char *previousToken, int allowPrompt) {
    @autoreleasepool {
        CNContactStore *store = [[CNContactStore alloc] init];
        CNAuthorizationStatus status = [CNContactStore authorizationStatusForEntityType:CNEntityTypeContacts];
        if (status == CNAuthorizationStatusNotDetermined && allowPrompt) {
            dispatch_semaphore_t wait = dispatch_semaphore_create(0);
            [store requestAccessForEntityType:CNEntityTypeContacts completionHandler:^(BOOL granted, NSError *error) { dispatch_semaphore_signal(wait); }];
            dispatch_semaphore_wait(wait, DISPATCH_TIME_FOREVER);
            status = [CNContactStore authorizationStatusForEntityType:CNEntityTypeContacts];
        }
        NSDictionary *output;
        if (status != CNAuthorizationStatusAuthorized) {
            output = @{@"error":@"请在系统设置中允许 Monica 读取通讯录。"};
        } else {
            CNChangeHistoryFetchRequest *request = [[CNChangeHistoryFetchRequest alloc] init];
            request.shouldUnifyResults = YES;
            request.includeGroupChanges = NO;
            request.additionalContactKeyDescriptors = @[
                [CNContactFormatter descriptorForRequiredKeysForStyle:CNContactFormatterStyleFullName],
                CNContactEmailAddressesKey, CNContactPhoneNumbersKey, CNContactOrganizationNameKey,
                CNContactJobTitleKey, CNContactBirthdayKey];
            if (previousToken && previousToken[0]) request.startingToken = [[NSData alloc] initWithBase64EncodedString:@(previousToken) options:0];
            NSError *error = nil;
            CNFetchResult<NSEnumerator<CNChangeHistoryEvent *> *> *result = [store enumeratorForChangeHistoryFetchRequest:request error:&error];
            if (!result && [error.domain isEqual:CNErrorDomain] &&
                (error.code == CNErrorCodeChangeHistoryExpired || error.code == CNErrorCodeChangeHistoryInvalidAnchor)) {
                request.startingToken = nil;
                error = nil;
                result = [store enumeratorForChangeHistoryFetchRequest:request error:&error];
            }
            if (!result) output = @{@"error":error.localizedDescription ?: @"通讯录读取失败"};
            else {
                MonicaContactsVisitor *visitor = [[MonicaContactsVisitor alloc] init];
                for (CNChangeHistoryEvent *event in result.value) [event acceptEventVisitor:visitor];
                output = @{@"records":visitor.records,@"reset":@(visitor.reset),@"token":[result.currentHistoryToken base64EncodedStringWithOptions:0] ?: @"",
                    @"coverage":@"这台 Mac 可读取的通讯录"};
            }
        }
        NSData *json = [NSJSONSerialization dataWithJSONObject:output options:0 error:nil];
        return strdup([[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding].UTF8String);
    }
}
