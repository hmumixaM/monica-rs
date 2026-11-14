# Monica Next → Rust 迁移覆盖清单

阶段性记录：数据库转换已完成；查询投影、完整性校验、文件复制和 NAS 部署验收仍在进行。此表不代表最终验收通过。

来源为四个分别取得的在线快照。每张源表均经过列清单、主键、数量和按稳定顺序计算的 SHA-256 指纹核对。完整清单和真实数据保存在私有迁移工作区，不提交到仓库。

| 来源库 | 源表 | 处理方式 | 快照记录数 |
| --- | --- | --- | ---: |
| crm | `activities` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,258 |
| crm | `activity_participants` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,503 |
| crm | `activity_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,276 |
| crm | `ai_outputs` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 18,583 |
| crm | `app_users` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1 |
| crm | `archive_media_files` | 派生重建；来源仍保留在不可变备份 | 259,093 |
| crm | `audit_events` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,038 |
| crm | `auth_login_attempts` | 历史档案；不安装为可执行任务或有效会话 | 1 |
| crm | `auth_recovery_codes` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 0 |
| crm | `community_names` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 0 |
| crm | `companies` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,989 |
| crm | `content_objects` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,432,912 |
| crm | `conversation_message_months` | 派生重建；来源仍保留在不可变备份 | 4,273 |
| crm | `conversation_sender_stats` | 派生重建；来源仍保留在不可变备份 | 13,935 |
| crm | `conversation_stats_dirty` | 派生重建；来源仍保留在不可变备份 | 0 |
| crm | `conversations` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,213 |
| crm | `data_version_events` | 派生重建；来源仍保留在不可变备份 | 3 |
| crm | `data_version_totals` | 派生重建；来源仍保留在不可变备份 | 10 |
| crm | `face_scans` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 124,721 |
| crm | `faces` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 122,019 |
| crm | `global_metric_snapshots` | 派生重建；来源仍保留在不可变备份 | 3,893 |
| crm | `group_membership_revisions` | 历史档案；不安装为可执行任务或有效会话 | 42,378 |
| crm | `group_memberships` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 15,530 |
| crm | `identities` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 31,191 |
| crm | `identity_candidates` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 0 |
| crm | `image_ocr_daily_usage` | 历史档案；不安装为可执行任务或有效会话 | 27 |
| crm | `image_ocr_readings` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 215,356 |
| crm | `image_ocr_settings` | 派生重建；来源仍保留在不可变备份 | 1 |
| crm | `import_batches` | 历史档案；不安装为可执行任务或有效会话 | 25,753 |
| crm | `import_request_nonces` | 历史档案；不安装为可执行任务或有效会话 | 1 |
| crm | `import_service_tokens` | 历史档案；不安装为可执行任务或有效会话 | 1 |
| crm | `jobs` | 历史档案；不安装为可执行任务或有效会话 | 292,444 |
| crm | `linkedin_collection_targets` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 414 |
| crm | `media_assets` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 254,893 |
| crm | `media_links` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 268,695 |
| crm | `media_manifest` | 历史档案；不安装为可执行任务或有效会话 | 33,955 |
| crm | `media_source_revisions` | 历史档案；不安装为可执行任务或有效会话 | 167,912 |
| crm | `media_uploads` | 历史档案；不安装为可执行任务或有效会话 | 0 |
| crm | `message_events` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,286,918 |
| crm | `message_search` | 派生重建；来源仍保留在不可变备份 | 2,286,918 |
| crm | `message_source_cursors` | 派生重建；来源仍保留在不可变备份 | 2,196 |
| crm | `model_call_slots` | 历史档案；不安装为可执行任务或有效会话 | 2 |
| crm | `person_weekly_updates` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,874 |
| crm | `platform_link_decisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 101 |
| crm | `platform_migration_state` | 派生重建；来源仍保留在不可变备份 | 2 |
| crm | `profile_address_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 697 |
| crm | `profile_dates` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 131 |
| crm | `profile_education_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,933 |
| crm | `profile_employment_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 4,561 |
| crm | `profile_field_current` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 47,889 |
| crm | `profile_field_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 73,528 |
| crm | `profile_metric_values` | 派生重建；来源仍保留在不可变备份 | 7,119 |
| crm | `profile_metrics_current` | 派生重建；来源仍保留在不可变备份 | 24,972 |
| crm | `profile_redirects` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 280 |
| crm | `profile_structured_current` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 5,739 |
| crm | `profile_suggestion_decisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 258 |
| crm | `profiles` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 16,395 |
| crm | `reconnect_states` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 7 |
| crm | `relationship_edge_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 16,976 |
| crm | `relationship_edges` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 16,880 |
| crm | `relationship_types` | 派生重建；来源仍保留在不可变备份 | 20 |
| crm | `remote_media_sources` | 历史档案；不安装为可执行任务或有效会话 | 167,911 |
| crm | `repair_snapshots` | 历史档案；不安装为可执行任务或有效会话 | 94,788 |
| crm | `scheduled_runs` | 历史档案；不安装为可执行任务或有效会话 | 773 |
| crm | `schema_migrations` | 派生重建；来源仍保留在不可变备份 | 77 |
| crm | `schools` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 502 |
| crm | `search_documents` | 派生重建；来源仍保留在不可变备份 | 0 |
| crm | `sessions` | 历史档案；不安装为可执行任务或有效会话 | 25 |
| crm | `social_comments` | 历史档案；不安装为可执行任务或有效会话 | 0 |
| crm | `social_interactions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 79,891 |
| crm | `social_post_locations` | 派生重建；来源仍保留在不可变备份 | 64,118 |
| crm | `social_post_media_readings` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 13,664 |
| crm | `social_post_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 147,468 |
| crm | `social_posts` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 64,118 |
| crm | `social_stories` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 5,544 |
| crm | `social_story_locations` | 派生重建；来源仍保留在不可变备份 | 5,544 |
| crm | `social_story_media_readings` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 4,843 |
| crm | `social_story_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 5,547 |
| crm | `source_account_facts` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 5,648 |
| crm | `source_account_fields` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 43,766 |
| crm | `source_account_link_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 12,895 |
| crm | `source_account_links` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 12,692 |
| crm | `source_account_media` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 29,141 |
| crm | `source_account_tags` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,013 |
| crm | `source_accounts` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 14,509 |
| crm | `source_chat_months` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 3,969 |
| crm | `source_contact_events` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 7,419 |
| crm | `source_crosswalk` | 历史档案；不安装为可执行任务或有效会话 | 11,834 |
| crm | `source_crosswalk_revisions` | 历史档案；不安装为可执行任务或有效会话 | 11,834 |
| crm | `source_cursors` | 历史档案；不安装为可执行任务或有效会话 | 569 |
| crm | `source_group_memberships` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 15,530 |
| crm | `source_observations` | 历史档案；不安装为可执行任务或有效会话 | 231,260 |
| crm | `source_profile_projections` | 派生重建；来源仍保留在不可变备份 | 90,440 |
| crm | `source_reconciliation` | 派生重建；来源仍保留在不可变备份 | 6 |
| crm | `source_relationship_edges` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 3,033 |
| crm | `source_runtime` | 历史档案；不安装为可执行任务或有效会话 | 13 |
| crm | `tag_membership_removals` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 54 |
| crm | `tag_membership_revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 9,779 |
| crm | `tag_memberships` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 7,066 |
| crm | `tags` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 37 |
| crm | `webauthn_challenges` | 历史档案；不安装为可执行任务或有效会话 | 0 |
| crm | `webauthn_credentials` | 历史档案；不安装为可执行任务或有效会话 | 0 |
| crm | `weekly_digests` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 5 |
| crm | `worker_heartbeats` | 历史档案；不安装为可执行任务或有效会话 | 99 |
| wechat | `account_identifiers` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 13,400 |
| wechat | `accounts` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 13,400 |
| wechat | `media` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 120,771 |
| wechat | `media_links` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 125,963 |
| wechat | `migration_cursors` | 历史档案；不安装为可执行任务或有效会话 | 1,190 |
| wechat | `objects` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,339,025 |
| wechat | `observations` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,317,461 |
| wechat | `payloads` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,077,033 |
| wechat | `revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,260,695 |
| instagram | `account_identifiers` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 1,633 |
| instagram | `accounts` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 549 |
| instagram | `media` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 15,731 |
| instagram | `media_links` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 15,930 |
| instagram | `migration_cursors` | 历史档案；不安装为可执行任务或有效会话 | 0 |
| instagram | `objects` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 22,573 |
| instagram | `observations` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 37,956 |
| instagram | `payloads` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 26,947 |
| instagram | `revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 32,355 |
| linkedin | `account_identifiers` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 854 |
| linkedin | `accounts` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 560 |
| linkedin | `media` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 2,389 |
| linkedin | `media_links` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 3,007 |
| linkedin | `migration_cursors` | 历史档案；不安装为可执行任务或有效会话 | 0 |
| linkedin | `objects` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 18,150 |
| linkedin | `observations` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 230,998 |
| linkedin | `payloads` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 19,730 |
| linkedin | `revisions` | 业务转换；未进入 Rust 活跃字段的元数据保留在历史档案 | 41,437 |

人物、消息、动态、原文、媒体及其历史保留原始 UUID。组织 ID 以及旧 UUID 账本对应的 Rust 序号使用确定映射。映射写入 `crm.legacy_id_map`。人工字段、删除覆盖、标签移除及关系优先级由 Rust 当前业务视图使用。来源事实随明确确认的账号关联移动。

Chatlog 另存快照清单。消息通过会话、服务器记录 ID（没有服务器 ID 时使用原始文件、表、local_id）与时间对账，保留已有消息 ID。内容修订保存独立版本及原始 CRM 基线。只读验证重新扫描独立快照，不能仅凭导入游标报告成功。

后续验收需补充：Chatlog 真实快照对账范围、关联媒体覆盖、源端缺失清单、文件逐项 SHA-256、最终审查提交、镜像摘要、NAS 两套登录及真实页面验证。
