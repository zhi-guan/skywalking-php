<?php

// Licensed to the Apache Software Foundation (ASF) under one or more
// contributor license agreements.  See the NOTICE file distributed with
// this work for additional information regarding copyright ownership.
// The ASF licenses this file to You under the Apache License, Version 2.0
// (the "License"); you may not use this file except in compliance with
// the License.  You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use Webmozart\Assert\Assert;

require_once dirname(__DIR__) . "/vendor/autoload.php";

extension_loaded('yar') or die("extension yar not loaded");

class YarEchoService
{
    public function ping($value)
    {
        Assert::notEmpty($_SERVER['HTTP_SW8'] ?? null);
        Assert::same($_SERVER['HTTP_X_SKYWALKING_TEST'] ?? null, 'yar');
        Assert::same($value, 'ok');

        return $value;
    }
}

$server = new Yar_Server(new YarEchoService());
$server->handle();
