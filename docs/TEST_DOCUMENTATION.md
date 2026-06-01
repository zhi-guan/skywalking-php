# skywalking-php 采样率与插件禁用功能 — 测试文档

> 版本: v1.1.0-patch | 日期: 2026-06-01

---

## 1. 测试环境

| 项目 | 值 |
|------|-----|
| PHP 版本 | 7.3.15 (NTS) |
| SAPI | FPM/FastCGI |
| 系统架构 | linux/amd64（通过 QEMU 在 macOS ARM64 上模拟） |
| 基础镜像 | rocketbird-mirror-registry.cn-shenzhen.cr.aliyuncs.com/rocketbird/php-fpm-basic:7.3-r6 |
| Rust 版本 | 1.85.1 |
| 容器编排 | Docker Compose v5.0.2 |
| 容器内组件 | nginx + php-fpm + redis-server + yar + supervisor |
| SkyWalking Agent | v1.1.0（本地编译含新功能） |

### 1.1 与生产环境差异

| 配置项 | 生产环境 | 测试环境 | 差异说明 |
|--------|---------|---------|---------|
| PHP 版本 | 7.3.15 | 7.3.15 | 一致 |
| SAPI | FPM/FastCGI | FPM/FastCGI | 一致 |
| session.save_handler | redis | redis | 一致 |
| Redis 扩展 | 5.2.0 | 5.2.0（同一基础镜像） | 一致 |
| Yar 扩展 | 2.1.2 | 2.1.2（pecl 安装） | 一致 |
| 上报目标 | 阿里云 OAP | 127.0.0.1:19876（无 OAP） | 不影响采样逻辑验证 |
| CPU 架构 | x86_64 | x86_64（QEMU 模拟） | 一致 |

---

## 2. 测试用例设计

### 2.1 测试 PHP 脚本（test.php）

每次请求执行以下操作：
1. `session_start()` — 触发 session Redis 读写（模拟生产噪音）
2. `Redis->connect/set/get/del` — 业务 Redis 调用
3. `curl_exec("http://127.0.0.1/index.php")` — HTTP 出口调用
4. `Yar_Client->ping()` — Yar RPC 调用（模拟业务间 RPC）

### 2.2 四轮测试矩阵

| 轮次 | sample_rate | disable_plugins | 测试目标 |
|------|-------------|-----------------|---------|
| 第一轮（基准） | `1.0` | `""` | 全量采集 + 全插件开启 |
| 第二轮（采样） | `0.0` | `""` | 完全关闭采集 |
| 第三轮（禁 Redis） | `1.0` | `"redis"` | 禁用 Redis 插件，验证其他插件不受影响 |
| 第四轮（禁 Yar） | `1.0` | `"yar"` | 禁用 Yar 插件，验证其他插件不受影响 |

### 2.3 验证指标

| 指标 | 检测方式 | 说明 |
|------|---------|------|
| 采样判断 | 日志 `should_sample check rate=X` | 确认 should_sample() 读取到正确的 rate 值 |
| 上下文错误 | 日志 `global tracing context not exists` | 无上下文 = 未创建 TracingContext = 采样关闭 |
| Redis 活跃 | 日志 `call redis method` / `call redis command` | 插件被触发 |
| Redis 禁用 | 无 `call redis` 日志 | 插件被过滤，未进入 hook |
| Yar 活跃 | 日志 `prepare yar client call` / `created yar exit span` | 插件被触发 |
| Yar 禁用 | 无 `prepare yar` 日志 | 插件被过滤，未进入 hook |
| Curl 正常 | 无 `curl_exec.*global tracing context not exists` 错误 | Curl span 创建成功 |
| 配置确认 | 日志 `Starting skywalking agent ... sample_rate=X disable_plugins=[...]` | Agent 启动时读取到正确配置 |

---

## 3. 测试执行流程

```
每轮测试流程：
  1. 写入 INI 配置到 /usr/local/etc/php/conf.d/skywalking-test.ini
  2. 清空 agent 日志（truncate -s 0）
  3. 重启 PHP-FPM（pkill → supervisor 自动拉起）
  4. 等待 6 秒（PHP-FPM 启动 + Agent 初始化）
  5. 验证 INI 配置生效（php -i | grep）
  6. 发送 10 次测试请求（curl http://127.0.0.1:8088/test.php）
  7. 等待 3 秒（日志刷盘）
  8. 读取并分析 agent 日志
```

---

## 4. 测试工具

### 4.1 自动化验证脚本（verify.sh）

- 路径：`test/verify/verify.sh`
- 用法：`cd test/verify && ./verify.sh`
- 自动执行 4 轮测试，输出 13 项 PASS/FAIL 结果
- 退出码 = 失败项数（0 = 全部通过）

### 4.2 手动验证命令

```bash
# 查看 agent 启动日志
docker compose exec app cat /tmp/skywalking-agent.log | grep "Starting skywalking"

# 查看 INI 配置
docker compose exec app php -i | grep -E "sample_rate|disable_plugins"

# 查看 Redis span 数量
docker compose exec app cat /tmp/skywalking-agent.log | grep -c "call redis"

# 查看 Yar span 数量
docker compose exec app cat /tmp/skywalking-agent.log | grep -c "prepare yar client call"

# 查看采样关闭错误
docker compose exec app cat /tmp/skywalking-agent.log | grep -c "global tracing context not exists"
```

---

## 5. 测试文件清单

| 文件 | 用途 |
|------|------|
| `Dockerfile_verify` | 基于 Dockerfile_test 编译镜像，添加 nginx/redis/yar/supervisor |
| `test/verify/docker-compose.yml` | 单容器编排 |
| `test/verify/supervisord.conf` | 管理 redis + php-fpm + nginx 进程 |
| `test/verify/nginx.conf` | nginx 反向代理 PHP-FPM |
| `test/verify/www/test.php` | 测试脚本（session + Redis + curl + Yar RPC） |
| `test/verify/www/index.php` | curl 目标端点 |
| `test/verify/www/yar.server.php` | Yar RPC 服务端（YarEchoService） |
| `test/verify/verify.sh` | 自动化 4 轮验证脚本（13 项检查） |

---

## 6. 可复现步骤

```bash
# 1. 编译基础镜像（含新代码）
cd /path/to/skywalking-php
docker build --platform linux/amd64 -f Dockerfile_test -t skywalking-verify-base .

# 2. 启动验证环境
cd test/verify
docker compose up -d --build

# 3. 等待就绪
sleep 5
curl http://127.0.0.1:8088/index.php  # 应返回 "ok"

# 4. 运行自动化验证
./verify.sh

# 5. 清理
docker compose down
```
