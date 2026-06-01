<?php
// Yar RPC Server - 提供 ping 方法供客户端调用
class YarEchoService
{
    public function ping($value)
    {
        return $value;
    }
}

$server = new Yar_Server(new YarEchoService());
$server->handle();
