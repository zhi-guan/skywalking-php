#!/bin/bash
# ==================================================================
# verify.sh - 验证 skywalking_agent 的 sample_rate 和 disable_plugins
# 用法: cd test/verify && ./verify.sh
# ==================================================================
set -euo pipefail

COMPOSE="docker compose"
APP_SVC="app"
APP_URL="http://127.0.0.1:8088"
INI_FILE="/usr/local/etc/php/conf.d/skywalking-test.ini"

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log()  { echo -e "${CYAN}[INFO]${NC} $*" >&2; }
ok()   { echo -e "${GREEN}[PASS]${NC} $*" >&2; }
fail() { echo -e "${RED}[FAIL]${NC} $*" >&2; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*" >&2; }

PASS_COUNT=0
FAIL_COUNT=0
pass() { PASS_COUNT=$((PASS_COUNT + 1)); ok "$*"; }
fail_count() { FAIL_COUNT=$((FAIL_COUNT + 1)); fail "$*"; }

count_pattern() {
    local log="$1"
    local pattern="$2"
    local count
    count=$(echo "$log" | grep -cE "$pattern" 2>/dev/null) || count=0
    count=$(echo "$count" | tr -d '[:space:]')
    [ -z "$count" ] && count=0
    echo "$count"
}

update_config() {
    local sample_rate="$1"
    local disable_plugins="$2"
    log "更新配置: sample_rate=$sample_rate, disable_plugins=\"$disable_plugins\""

    $COMPOSE exec -T "$APP_SVC" bash -c "cat > $INI_FILE << 'INIEOF'
[skywalking_agent]
skywalking_agent.enable = 1
skywalking_agent.server_addr = \"127.0.0.1:19876\"
skywalking_agent.service_name = \"verify-test\"
skywalking_agent.log_level = \"DEBUG\"
skywalking_agent.log_file = \"/tmp/skywalking-agent.log\"
skywalking_agent.sample_rate = $sample_rate
skywalking_agent.disable_plugins = \"$disable_plugins\"
skywalking_agent.inject_context = 1
skywalking_agent.worker_threads = 2
INIEOF"

    $COMPOSE exec -T "$APP_SVC" bash -c "> /tmp/skywalking-agent.log"
    log "重启 PHP-FPM ..."
    $COMPOSE exec -T "$APP_SVC" pkill php-fpm || true
    sleep 6
    log "验证 INI..."
    $COMPOSE exec -T "$APP_SVC" php -i 2>/dev/null | grep -E "sample_rate|disable_plugins" | head -4 || true
}

send_requests() {
    local count="$1"
    local success=0
    log "发送 $count 次测试请求..."
    for i in $(seq 1 "$count"); do
        HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${APP_URL}/test.php" 2>/dev/null || echo "000")
        [ "$HTTP_CODE" = "200" ] && success=$((success + 1))
        sleep 0.05
    done
    log "请求完成: $success/$count 成功"
    echo "$success"
}

get_log() {
    $COMPOSE exec -T "$APP_SVC" cat /tmp/skywalking-agent.log 2>/dev/null || echo ""
}

# ================================================================
echo "" >&2
echo "============================================================" >&2
echo "  skywalking_agent 功能验证" >&2
echo "============================================================" >&2

log "检查服务状态..."
if ! curl -sf "${APP_URL}/index.php" > /dev/null 2>&1; then
    fail "app 未就绪"; exit 1
fi
ok "服务就绪"

# ==================== 第一轮：全量采集 ====================
echo "" >&2
echo "============================================================" >&2
echo "  第一轮：sample_rate=1.0, disable_plugins=\"\" (基准)" >&2
echo "============================================================" >&2

update_config "1.0" ""
REQ1=$(send_requests 10)
sleep 3
LOG1=$(get_log)

# 采样正常：should_sample rate=1 出现
SAMPLED1=$(count_pattern "$LOG1" "should_sample check rate=1")
# 无 "global tracing context not exists" 错误 → span 创建成功
CTX_ERR1=$(count_pattern "$LOG1" "global tracing context not exists")
# curl 插件成功创建 span（无错误）
CURL_ERR1=$(count_pattern "$LOG1" 'curl_exec.*global tracing context not exists')
# Redis 插件成功创建 span（无错误）
REDIS_ERR1=$(count_pattern "$LOG1" 'Redis.*global tracing context not exists')
# Redis 插件被调用
REDIS_DBG1=$(count_pattern "$LOG1" "call redis method|call redis command")
# Yar 插件被调用
YAR_DBG1=$(count_pattern "$LOG1" "prepare yar client call|created yar exit span")

echo "" >&2
echo "  请求: $REQ1/10 | 采样检查: $SAMPLED1 | 上下文错误: $CTX_ERR1" >&2
echo "  Redis: $REDIS_DBG1 | Curl错误: $CURL_ERR1 | Redis错误: $REDIS_ERR1 | Yar: $YAR_DBG1" >&2
echo "" >&2

if [ "$SAMPLED1" -ge 8 ]; then
    pass "第一轮：采样正常 ($SAMPLED1 次 rate=1.0 检查)"
else
    fail_count "第一轮：采样异常 ($SAMPLED1 次)"
fi
if [ "$CTX_ERR1" -eq 0 ]; then
    pass "第一轮：无上下文错误 (span 创建成功)"
else
    fail_count "第一轮：存在 $CTX_ERR1 个上下文错误"
fi
if [ "$REDIS_DBG1" -gt 0 ]; then
    pass "第一轮：Redis 插件活跃 ($REDIS_DBG1 次调用)"
else
    warn "第一轮：Redis 插件无调用"
fi
if [ "$YAR_DBG1" -gt 0 ]; then
    pass "第一轮：Yar 插件活跃 ($YAR_DBG1 次调用)"
else
    warn "第一轮：Yar 插件无调用"
fi

# ==================== 第二轮：关闭采样 ====================
echo "" >&2
echo "============================================================" >&2
echo "  第二轮：sample_rate=0.0 (不采集)" >&2
echo "============================================================" >&2

update_config "0.0" ""
REQ2=$(send_requests 10)
sleep 3
LOG2=$(get_log)

SAMPLED2=$(count_pattern "$LOG2" "should_sample check rate=0")
CTX_ERR2=$(count_pattern "$LOG2" "global tracing context not exists")
INIT_ERR2=$(count_pattern "$LOG2" "request init failed.*global tracing context not exists")
# Yar 在关闭采样时不应创建 span
YAR_DBG2=$(count_pattern "$LOG2" "prepare yar client call|created yar exit span")

echo "" >&2
echo "  请求: $REQ2/10 | rate=0检查: $SAMPLED2 | 上下文错误: $CTX_ERR2 | init错误: $INIT_ERR2" >&2
echo "  Yar调用: $YAR_DBG2 (应为0)" >&2
echo "" >&2

if [ "$SAMPLED2" -ge 8 ]; then
    pass "第二轮：rate=0.0 正确识别 ($SAMPLED2 次)"
else
    fail_count "第二轮：rate=0.0 未识别 ($SAMPLED2 次)"
fi
# 关键验证：所有插件调用都因无上下文而失败 = 没有创建任何 span
if [ "$CTX_ERR2" -gt 0 ]; then
    pass "第二轮：无 span 被创建 ($CTX_ERR2 个上下文不存在错误 = 采样已关闭)"
else
    fail_count "第二轮：未检测到上下文错误 (采样可能未生效)"
fi

# ==================== 第三轮：禁用 Redis ====================
echo "" >&2
echo "============================================================" >&2
echo "  第三轮：sample_rate=1.0, disable_plugins=\"redis\"" >&2
echo "============================================================" >&2

update_config "1.0" "redis"
REQ3=$(send_requests 10)
sleep 3
LOG3=$(get_log)

SAMPLED3=$(count_pattern "$LOG3" "should_sample check rate=1")
REDIS_DBG3=$(count_pattern "$LOG3" "call redis method|call redis command")
CURL_ERR3=$(count_pattern "$LOG3" 'curl_exec.*global tracing context not exists')
# Yar 在禁用 redis 时仍应正常工作
YAR_DBG3=$(count_pattern "$LOG3" "prepare yar client call|created yar exit span")
DISABLED3=$(echo "$LOG3" | grep 'disable_plugins=\["redis"\]' 2>/dev/null | wc -l | tr -d '[:space:]')

echo "" >&2
echo "  请求: $REQ3/10 | 采样: $SAMPLED3 | Redis调用: $REDIS_DBG3 | Curl错误: $CURL_ERR3 | Yar: $YAR_DBG3" >&2
echo "  disable_plugins 配置确认: $DISABLED3" >&2
echo "" >&2

if [ "$DISABLED3" -gt 0 ]; then
    pass "第三轮：disable_plugins 正确加载"
else
    fail_count "第三轮：disable_plugins 未正确加载"
fi
if [ "$REDIS_DBG3" -eq 0 ]; then
    pass "第三轮：Redis 插件完全禁用 (0 次调用)"
else
    fail_count "第三轮：Redis 仍有 $REDIS_DBG3 次调用"
fi
if [ "$CURL_ERR3" -eq 0 ]; then
    pass "第三轮：Curl 插件正常 (无错误)"
else
    warn "第三轮：Curl 有 $CURL_ERR3 个错误"
fi
if [ "$YAR_DBG3" -gt 0 ]; then
    pass "第三轮：Yar 插件不受 redis 禁用影响 ($YAR_DBG3 次调用)"
else
    warn "第三轮：Yar 插件无调用"
fi

# ==================== 第四轮：禁用 Yar ====================
echo "" >&2
echo "============================================================" >&2
echo "  第四轮：sample_rate=1.0, disable_plugins=\"yar\"" >&2
echo "============================================================" >&2

update_config "1.0" "yar"
REQ4=$(send_requests 10)
sleep 3
LOG4=$(get_log)

SAMPLED4=$(count_pattern "$LOG4" "should_sample check rate=1")
YAR_DBG4=$(count_pattern "$LOG4" "prepare yar client call|created yar exit span")
# Redis 应该正常工作（只禁用了 yar）
REDIS_DBG4=$(count_pattern "$LOG4" "call redis method|call redis command")
DISABLED4=$(echo "$LOG4" | grep 'disable_plugins=\["yar"\]' 2>/dev/null | wc -l | tr -d '[:space:]')

echo "" >&2
echo "  请求: $REQ4/10 | 采样: $SAMPLED4 | Yar调用: $YAR_DBG4 | Redis: $REDIS_DBG4" >&2
echo "  disable_plugins 配置确认: $DISABLED4" >&2
echo "" >&2

if [ "$DISABLED4" -gt 0 ]; then
    pass "第四轮：disable_plugins=yar 正确加载"
else
    fail_count "第四轮：disable_plugins 未正确加载"
fi
if [ "$YAR_DBG4" -eq 0 ]; then
    pass "第四轮：Yar 插件完全禁用 (0 次调用)"
else
    fail_count "第四轮：Yar 仍有 $YAR_DBG4 次调用"
fi
if [ "$REDIS_DBG4" -gt 0 ]; then
    pass "第四轮：Redis 插件不受 yar 禁用影响 ($REDIS_DBG4 次调用)"
else
    warn "第四轮：Redis 插件无调用"
fi

# ==================== 汇总 ====================
echo "" >&2
echo "============================================================" >&2
echo "  验证结果汇总" >&2
echo "============================================================" >&2
echo "" >&2

if [ "$FAIL_COUNT" -eq 0 ]; then
    echo -e "  ${GREEN}通过: $PASS_COUNT    失败: $FAIL_COUNT${NC}" >&2
    ok "全部验证通过!"
else
    echo -e "  ${RED}通过: $PASS_COUNT    失败: $FAIL_COUNT${NC}" >&2
    fail "$FAIL_COUNT 项验证失败"
fi

echo "" >&2
echo "  详细日志: docker compose exec app cat /tmp/skywalking-agent.log" >&2
echo "" >&2
exit $FAIL_COUNT
