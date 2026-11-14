# 关联页头像查询索引

关联列表会按账号寻找所属头像。旧查询没有对应索引时，每个账号会顺序扫描完整
`crm.media_links`；在已有数据的 NAS 上，这条查询触发了 statement timeout。
迁移 0071 为其添加账号、角色、媒体 ID 的部分索引。隐藏的列表也不再自动请求，
切换回来仍保留筛选、选择和已缓存结果；错误状态可重新读取。

完整版本部署前，可只对已确认身份的 Rust 数据库预建同名索引，不改变旧版数据库、
容器或 `_sqlx_migrations`，无需重启服务。必须使用审查后的定义及只属于 Rust 的连接。
先核对 PostgreSQL system identifier、数据库 OID、SSD 挂载和现有服务身份；存在同名索引时
检查有效性及定义，不能盲目重建或删除。不要在事务块内执行 CONCURRENTLY。

```sql
SET maintenance_work_mem = '16MB';
SET max_parallel_maintenance_workers = 0;
SET lock_timeout = '2s';
SET statement_timeout = '2min';
CREATE INDEX CONCURRENTLY IF NOT EXISTS media_links_account_avatar
 ON crm.media_links(source_account_id,role,media_id)
 WHERE source_account_id IS NOT NULL
 AND role IN ('wechat_avatar_small','wechat_avatar','instagram_avatar','linkedin_avatar');
```

这只读写 Rust SSD 中的媒体关联及新索引，不扫描原始媒体文件。执行前后检查复制心跳和
NAS 内存。未完成、无效或定义不同的同名索引须报告并保留证据；不能当作成功跳过。
预建成功后记录 SQL 校验值、索引定义、indisvalid/indisready 和实际查询计划。
后续正式迁移仍正常执行并登记 0071；IF NOT EXISTS 复用已核实的同名索引。
