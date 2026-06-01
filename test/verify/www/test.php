<?php
// 测试脚本：模拟业务中的 Redis + curl + Session 调用
// 用于验证 sample_rate 和 disable_plugins 功能

session_start();

$result = [];

// ===== 1. Session 操作（底层走 Redis，模拟生产环境噪音） =====
$_SESSION['test'] = 'value_' . time();
$result['session_id'] = session_id();

// ===== 2. 业务 Redis 调用 =====
try {
    $redis = new Redis();
    $redis->connect('127.0.0.1', 6379);
    $redis->set('test_key', 'hello_' . time());
    $result['redis_get'] = $redis->get('test_key');
    $redis->del('test_key');
    $result['redis'] = 'ok';
} catch (Exception $e) {
    $result['redis'] = 'error: ' . $e->getMessage();
}

// ===== 3. curl 调用（HTTP 出口） =====
$ch = curl_init();
curl_setopt($ch, CURLOPT_URL, "http://127.0.0.1/index.php");
curl_setopt($ch, CURLOPT_TIMEOUT, 5);
curl_setopt($ch, CURLOPT_RETURNTRANSFER, 1);
$resp = curl_exec($ch);
$httpCode = curl_getinfo($ch, CURLINFO_HTTP_CODE);
curl_close($ch);
$result['curl'] = $resp === 'ok' ? 'ok' : "error: http=$httpCode";

// ===== 4. Yar RPC 调用（模拟业务间 RPC） =====
try {
    $yarClient = new Yar_Client("http://127.0.0.1/yar.server.php");
    $yarResult = $yarClient->ping('yar_test');
    $result['yar'] = ($yarResult === 'yar_test') ? 'ok' : 'error: unexpected result';
} catch (Exception $e) {
    $result['yar'] = 'error: ' . $e->getMessage();
}

// ===== 5. 输出 =====
header('Content-Type: application/json');
echo json_encode($result, JSON_PRETTY_PRINT);
