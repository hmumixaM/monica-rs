# 群聊图片补齐

该功能默认关闭。数据库迁移、打开导入中心或启动 Rust 服务均不会扫描或下载图片。
本次现有数据导入完成之前，不运行补齐任务。

导入中心的“微信群聊图片”显示每轮扫描、账号、按群和 UTC 日期的 coverage，以及稳定的消息 ID。
它区分图片可用、原图、高清、标准、缩略图、清晰度未确认、未找到缓存、缺少原文引用、
解码失败和消息变更。扫描消息尚未结束时不展示百分比。历史没有运行记录时显示“无记录”。
本轮扫描结束不等于所有历史原图已取得。

## 图片来源与范围

`scripts/wechat-group-images.py` 扫描指定账号的全部群聊图片消息，包括没有关联人物的群，
也识别低 32 位类型为 3 的微信消息。不重新导入消息或人物，不修改发送者、归档、人工值或原文。
消息撤回、删除或原文变更会在关联图片之前再次核对。

图片从该账号的 `msg` 目录或其独立副本读取。Linux 必须将来源挂载为只读。
扫描全部 `Img` 缓存目录，以原文 XML 的图片 MD5 关联；可用冻结的 hardlink 元数据索引补充
文件名与 MD5 不同的别名。账号隔离，不根据相同昵称、消息正文中的任意哈希或发送者猜测关联。
既有媒体通过当前原文图片引用核对后复用；发现更高等级缓存时才补充。
原文换图时不会把旧附件误报成当前图片。只解除该功能自己生成的过期公开关联，
原有导入／手工关联、媒体文件和来源证据保留。

这不是微信云端下载协议。微信从未缓存的历史图片，需要先在微信中取得，再同步进只读来源。
任务不调用 Chatlog 的图片 HTTP 路由：部分 Chatlog 版本会在这些 GET 请求中写回解密文件，
甚至尝试刷新微信进程中的密钥。补齐只用既有离线解码器，在 Rust 专属媒体目录生成文件。
不启动微信、原版 Monica、采集器、模型、OCR 或人脸分析。

文件名后缀 `_h`/`_hd` 标为高清，`_b` 标为标准，`_t`/`_thumb` 标为缩略图；
没有明确依据时标为“清晰度未确认”。只有显式 `original` 文件名标记才报告原图。
不能从像素尺寸推断已取得原图。

## 安装和执行（导入完成后）

使用 Python 3.10+、`psycopg[binary]==3.2.12` 和
[现有离线解码器](../tools/migration-media/README.md)。解码器需要单独构建，不属于主服务容器。
目标数据库需已应用迁移 0068。数据库账号需要目标库写权限和读取 `pg_control_system()` 的权限。
准备独立的 state 目录；媒体根目录必须有既有隔离标记
`.monica-rust-migration-media`，内容是 `monica-rust-isolated-media-v1` 加换行。
state、source、media 三个目录必须互不重叠，且路径不能经过符号链接。

没有 `--run` 时，命令只打印帮助，不连接数据库或读取图片。执行前在导入中心允许补齐。
本机管理者明确提供目标库名和 PostgreSQL system identifier；不自动发现旧版数据库作为目标。

```sh
python3 scripts/wechat-group-images.py --run \
  --account '<crm.conversations.account 中的准确账号>' \
  --source-root /source/msg \
  --media-root /target/media \
  --state-root /target/group-image-state \
  --decoder /opt/monica/media-decode \
  --expected-database monica_rust \
  --expected-system-id '<Rust PostgreSQL system identifier>'
```

原生连接的 `crm.conversations.account` 是 connection UUID，来源微信账号则是 wxid。
这种情况必须另加 `--source-owner '<微信账号 wxid>'`，并与该连接保存的 `external_account` 一致。
冻结索引校验的是 source-owner；目标查询使用 account，两者不可混为同一个标识。
旧版迁移账号没有原生 connection 时，source-owner 直接采用目标 account，不允许跨账号覆盖。

连接凭据只通过环境变量 `MONICA_DATABASE_URL` 提供。
有需要时，已有图片解密凭据只通过 `MONICA_MIGRATION_IMAGE_KEY` 和
`MONICA_MIGRATION_IMAGE_XOR` 交给解码子进程，不写日志、参数、清单或数据库。
可加 `--file-index /metadata/wechat-file-index.sqlite3` 读取冻结索引；索引必须标记完整，且账号一致。
它只提供候选路径，文件还需通过解码和内容校验。

仅启动一个任务。NAS 上应通过独立受限容器或 detached 进程运行，来源只读、目标独立，
不依赖 Mac 的 SSH 会话。任务设为单 worker，平均读取／写出预算 2 MiB/s；
Linux 可用内存低于 768 MiB 时暂停。部署时另加内存及 CPU 限额。
该任务是独立的手动维护工具；主服务不会自动启动它。网页上的“允许补齐”只控制已配置任务的执行权限。

暂停／关闭会在下一个安全边界停止；单个解码最多 45 秒。重新运行同一条命令可续跑，
SQLite 索引和 PostgreSQL 项目状态一起避免重复统计。完成后若来源增加了图片，
加 `--retry` 可重查已处理条目、升级缩略图；同一内容仍复用原媒体，缺失项仍保留解释。
文件和目录持久化、SHA-256 核对完成后才建立消息关联。失败保留进度和原数据。

## 本地验证

```sh
python3 scripts/test-wechat-group-images.py
# 可选：仅在可创建临时数据库的本地测试 PostgreSQL 上验证完整流程。
MONICA_GROUP_IMAGE_TEST_ADMIN='<本地专用测试库连接>' \
  uv run --with 'psycopg[binary]==3.2.12' scripts/test-wechat-group-images.py
```

数据库验证创建、使用并删除随机命名的 `monica_rust_group_images_test_*` 临时库。
用合成图片／消息验证默认关闭、未关联群、跨账号隔离、别名、高清失败回退、续跑、
重复执行、高清升级、暂停以及原数据保留。不访问真实微信、NAS 或模型服务。
