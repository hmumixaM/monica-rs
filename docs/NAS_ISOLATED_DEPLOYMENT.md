# Monica Rust 独立 NAS 部署

本流程只操作新项目 `monica-rs`。旧版 Monica Next 的数据库、运行目录、容器、代理、采集器、授权和调度配置继续保持原样。当前文档描述部署流程；实际验收记录尚未完成。

新配置目录为 `~/docker/monica-rs`，新数据根目录为 `/volume1/docker/monica-rs`。HTTPS 入口为 `https://nas.tailfb022c.ts.net:9444`。数据库不发布主机端口，数据库和服务使用独立内部网络；只有 TLS 代理额外加入本项目的入口网络，以便发布 Tailscale 地址上的 9444 端口。运行容器只挂载新目录。

`infra/nas.compose.yaml` 要求明确提供 Rust、PostgreSQL 和 Caddy 的不可变镜像引用。Rust 镜像从已完成本地技能审查并合并的 main 提交构建。交付记录同时保存源码提交、多架构镜像摘要和 amd64 实际镜像摘要。

Monica Rust 是这台 NAS 上的主服务，三个容器使用全部四个核心，不再固定 cpuset 或降低调度权重。PostgreSQL 上限 3 GiB（`shared_buffers=1GB`，可用 `MONICA_PG_SHARED_BUFFERS`、`MONICA_PG_MEMORY` 调整），Rust 服务 1 GiB，代理 128 MiB。数据库比内存大得多，热数据要留在 shared buffers 里；存储按 SSD 计价（`random_page_cost=1.1`），超过 2 秒的语句写入日志但不记录绑定参数。所有 PostgreSQL 参数都在 `infra/nas.compose.yaml` 的启动命令里，不使用 `ALTER SYSTEM`。`pg_prewarm` 会定期记录缓存里有哪些页，重启后自动读回；否则每次重启后的头一段时间，页面请求都要逐页从磁盘读数据。安装秘密、控制 token、导入凭据和数据库密码均独立生成。

PostgreSQL 的 Unix socket 目录是数据根目录下的 `postgres-run`（宿主机目录）。导入 worker 把它只读挂载到 `/pg`，因此重建数据库容器不会让 worker 失去连接。数据目录和 socket 目录都设置了 `create_host_path: false`：数据盘没有挂载时容器会启动失败，而不是在别处初始化一个空库。

数据盘是 USB 外接 SSD，挂载晚于 Docker 启动。所有 bind mount 都设置了 `create_host_path: false`，磁盘未挂载时容器启动失败，不会在系统盘上建出空目录。`infra/nas-boot.sh` 安装为 `/usr/local/etc/rc.d/monica-rs.sh`，开机后等待磁盘和 Docker 就绪，然后依次：把该设备设为 `rotational=0` 和 `deadline`（DSM 内核会把它当作机械盘并使用 CFQ 调度，交互查询会排在后台查询的时间片后面）、`compose up` 三个服务、启动导入 worker 容器（名称记录在 `config/live-imports.container`），以及两个宿主机辅助进程（`config/chatlog-readiness.py` 和 `config/wechat-images-supervisor.py`，后者的参数在 `config/wechat-images.args`）。SSD 必须接在 USB 3 口并使用 USB 3 线，`cat /sys/bus/usb/devices/*/speed` 中该设备应为 5000。

辅助进程通过容器名和可执行文件路径找到 PostgreSQL 和 Chatlog，不依赖进程号、容器 ID 或设备号，重启后无需修改。

表结构迁移用 `docker-compose --profile migrate run --rm migrate` 执行，然后再启动 `server`。

开始启动服务前，再检查 9444 端口空闲，以及当前内存、磁盘等待和旧版健康状态。用户后续明确取消固定的 1.5 GiB / 五分钟启动条件，要求以 NAS 稳定为准，并允许必要时暂时停止 Monica Next 进行 Rust 测试。先暂停新增复制工作，再逐项启动受内存限制的新服务；出现压力时只停止本次新增任务。若临时停旧版，应记录准确容器 ID 和原运行状态，并在测试后恢复。复制仍使用保守的 768 MiB 保护值，可根据实测资源情况调整。

## 取得和转换副本

1. 记录旧版入口、运行镜像、容器 ID、启动时间、挂载、健康检查及延迟基线。四个数据库逐个运行 `scripts/nas-migration-export.py`，从在线只读事务逻辑备份直接写到 Mac 专用目录。每个库独立记录时间及 SHA-256，分别取得的快照具有不同采样时间。
2. 在 Mac 专用 PostgreSQL 实例恢复四库。不要复用 Mac 应用的数据库、端口、数据目录或安装秘密。迁移源连接采用只读模式；转换工具要求本机 Unix socket，并核对目标的系统标识、数据库身份及空目标。
3. 使用 `scripts/legacy-import.py inspect/apply/verify --manifest PATH`。四个源连接放在 `MONICA_LEGACY_{CRM,WECHAT,INSTAGRAM,LINKEDIN}_URL`，目标放在 `MONICA_DATABASE_URL`。安装秘密使用 `MONICA_SESSION_SECRET`；只有源账号存在 TOTP 时才需要临时提供旧安装秘密。不要将秘密放入命令参数或日志。
4. `monica-server legacy-finalize MIGRATION_UUID` 重建会话统计、月份、摘要侧栏、来源投影基线、人物资料及确定性 Memory 输入。它使用独立管理连接和有界语句期限；原始历史任务保持历史状态。再次运行验证，确认每张表的内容、映射与当前值。
5. Chatlog 活跃 SQLite 使用 `scripts/nas-migration-copy.py chatlog-db` 创建一致性备份和 SHA-256。源挂载只读，复制后立即释放 WAL 读快照。使用 `scripts/nas-migration-download.py` 取回新目录中的快照，在 Mac 使用 `legacy-chatlog-import.py inspect` 执行完整性检查，然后执行 `apply` 和只读 `verify`。已提交批次需要按修复后的规则重新对账时，使用显式 `reconcile`；它完整扫描冻结副本、保留旧收据和修订，并在独立的小型扫描状态中断点继续。Chatlog 应用完毕还会强制执行第二次确定性结算；会话统计和 Memory 输入未就绪时，不会标记转换完成。不得将一次普通文件复制视作活跃 SQLite 的有效备份。所有 SQLite 表通过固定规则分类；未知文件和表会阻止迁移完成。业务表核对完整扫描数量，历史表在 Rust 中分页归档并逐行复核，授权和可执行运行状态不会安装为有效凭据或队列。

迁移覆盖清单见 [NAS_MIGRATION_COVERAGE.md](NAS_MIGRATION_COVERAGE.md)。未分类表、未解析引用、正文或内容哈希差异会中止转换或验收。Chatlog 的修订保留原消息 ID、原始正文和独立版本；验证既检查原始 CRM 基线，也检查最新快照对应的当前投影。

## 文件及运行隔离

`scripts/nas-migration-copy.py` 只接受明确列出的旧版业务目录。源通过只读 bind mount 进入无网络的临时工作容器。目标、日志、SQLite 进度账本、临时文件和暂停记录全部位于新目录。目录校验拒绝源目标重叠和指向其他目录的符号链接。读取旧目录的临时容器仅获得 DAC_READ_SEARCH 读能力；源挂载仍强制只读。新数据外层根目录保持 root:root、0700；只读 bind mount 内的档案和遥测目录使用可供 UID 10001 读取的权限。下载器通过受限 sudo 命令只读取新目录。

同一时刻只运行一个 NAS 批量任务，初始限速 2 MiB/s。旧版连续健康失败、明显延迟恶化或可用内存跌破门槛时，只停止新增工作。数据库导出只取消独立应用名称对应的导出事务；文件复制只停止具有完全匹配标签的新工作容器。`--resume` 重新检查健康和内存，并保留原暂停记录。

媒体、原始档案、暂存、导出及备份先复制到 `snapshots/monica-next`。Chatlog 数据库独立保存在 `snapshots/chatlog`。运行时媒体目录只放入已登记的内容寻址文件，完整归档位于运行时垃圾回收范围外。禁止从旧目录建立硬链接；新归档到新运行目录可使用独立 inode 的 Btrfs reflink。

`MONICA_COOKIE_PREFIX=monica_rs` 覆盖登录、退出、CSRF、会话管理和 OAuth 会话读取。旧版的 `crm_*` Cookie 保持原样。前端从本服务的 CSRF 接口取得自己的 token。旧会话和采集器令牌只作为脱敏历史保存，不安装为 Rust 的有效凭据。

## 备份与恢复

最终验收通过后，对新 PostgreSQL 做完整逻辑备份，将配置、镜像引用、源码提交和媒体校验清单另存到新备份目录。备份时不包含临时服务凭据、浏览器会话或 Chatlog 全局解密密钥。业务账号的密码哈希保留；TOTP 使用新安装秘密重新加密，因此恢复时应保留 Rust 自己的安装秘密。

恢复到另一个新的 `monica-rs` 数据根目录和独立数据库，将完整逻辑备份恢复到空数据库（备份中已含结构和迁移记录），再使用同一源码版本运行迁移校验，随后恢复文件。核对镜像、迁移校验值、文件哈希和业务对账结果。保持采集及模型执行关闭，完成恢复验收后再开放入口。

回退仅停止新项目，例如在新配置目录使用 `docker-compose -p monica-rs stop`。保留新数据与迁移证据。不要使用 `down -v`，不要清理旧目录或操作旧版 Compose 项目。

SQLite 一致性备份的行为依据其[官方 Online Backup API 文档](https://www.sqlite.org/c3ref/backup_finish.html)。NAS 上的备份负责一致性和字节记录；完整 `quick_check` 在独立 Mac 副本完成后，才能进入业务导入。

Chatlog apply 在标记转换完成前自动调用 Rust `legacy-finalize`，刷新会话、月份、侧栏摘要和确定性 Memory 输入；verify 也核对这些独立读取投影。可用 `--server-binary` 或 `MONICA_SERVER_BINARY`（兼容 `MONICA_LEGACY_FINALIZER`）指定同一源码版本的 Rust 可执行文件。此流程不调用模型。新数据总根保持 root/0700；挂载的档案、遥测和复制暂存目录使用服务组 10001 的 setgid 只读权限。下载器通过环境 `NAS_PASSWORD` 向限定新目录的读取命令提供 sudo stdin 凭据。

后续本地 Chatlog 快照中消失的行只形成 `snapshot_record_absent` 差异证据。快照缺失不能证明上游删除，因此历史消息、联系人和动态仍保留；显式撤回通知继续按原有版本化路径处理。

Reader 的本地进度与 ACK 绑定到具体目标 PostgreSQL 实例、数据库及来源身份。换目标时需指定新 `--state` 目录。旧状态只有在每条 ACK 均能在当前目标库找到匹配提交收据时才允许补建目标绑定，避免将另一份副本的已读进度当作当前目标已入库的证明。
