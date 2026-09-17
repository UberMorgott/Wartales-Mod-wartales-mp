# wartales-mp — замена сетевого коннекта Wartales

Цель: коннект между игроками идёт напрямую, минуя инфраструктуру Shiro и Steam-релеи.
Игра не патчится. Байткод (`hlboot.dat`), паки и `steam.hdll` не трогаем.

## Как игра соединяется сейчас (установлено реверсом, см. ../decomp)

1. `Main.initMpman@22323` жёстко задаёт мастер: `master.shirogames.com` / `master2.shirogames.com`,
   порт `60442`, `ssl:true`, game `"wartales"` (`getMPManConfig@14965`).
2. Мастер — WebSocket, текстовые фреймы `haxe.Json.print({uid, cmd, args})`
   (`WSConnection.sendCommand@54831`, разбор `onJSON@54829`).
3. Транспорт выбирает НЕ игра, а мастер: `Lobby.setupPlatform@24597` → `ServerQuery.query@26420`
   шлёт `instance/get`, читает из ответа `serverID` и `serverStartAnswer.hostpw/.slavepw`.
   Первый символ `serverID` задаёт платформу (`UserID.getPlatform@25195`):
   `R<host>:<port>[S]` → хост становится RelayP2P(11), клиент — WServer(9).
   Любой другой префикс на этом пути → «Invalid platform».
4. Relay-протокол: тот же WebSocket, бинарные фреймы, заголовок 3 байта `[type:u8][cid:u16]`,
   `1`=connect, `2`=disconnect, `3`=data; хост входит с ident, начинающимся на `@:`, паролем `hostpw`,
   клиенты — `slavepw`. Первый `@`-коннект становится хостом.
5. Короткий код подключения выдаёт мастер: `lobby/makeShortCode` → `{shortCode}`,
   разрешает `lobby/resolveShortCode` → LobbyInfo. На Steam-протоколе не реализовано вовсе.
6. TLS обязателен и строгий: `AsyncSocket.init@54846` ставит `verifyCert = true`,
   CA грузится из хранилища Windows (`cert_load_defaults@59352`), имя сверяется
   с `master.shirogames.com` (`ssl_set_hostname@59362`).

Вывод: кто контролирует мастер — тот контролирует транспорт. Патч игры не нужен.

## Архитектура решения

Один исполняемый файл `wartales-mp.exe` у каждого игрока. Внутри три части:

| Часть | Слушает | Назначение |
| --- | --- | --- |
| master | `127.0.0.1:60442`, TLS | подменяет мастер Shiro для локальной игры |
| relay | публичный TCP-порт (по умолчанию `14250`) | мост между играми хоста и гостей |
| proxy-link | тот же публичный порт | мастер-команды гостя уходят мастеру хоста |

Публичный порт один и мультиплексируется по первым байтам соединения:
`GET` с пробелом → relay (WebSocket), иначе → proxy-link (построчный JSON).

### Роль хоста

1. Игра логинится в наш локальный мастер, создаёт лобби — состояние лобби хранится у хоста,
   он авторитетный источник.
2. `instance/get` → отвечаем `serverID = "R127.0.0.1:<relayPort>"` (без `S`, TLS на relay не нужен)
   плюс сгенерированные `hostpw`/`slavepw`. Игра хоста подключается к нашему relay как хост.
3. `lobby/makeShortCode` → узнаём внешний адрес (UPnP `AddPortMapping` + `GetExternalIPAddress`,
   при неудаче — STUN `stun.l.google.com:19302`), кодируем `ip:port` в короткий код и отдаём игре.

### Роль гостя

1. До ввода кода мастер отвечает локально (login/session/time), чтобы меню работало.
2. Ввод кода → `lobby/resolveShortCode`: декодируем код в `ip:port` хоста, поднимаем proxy-link
   к мастеру хоста и с этого момента ПРОКСИРУЕМ все `lobby/*` туда. Состояние лобби едино.
3. `instance/get` → `serverID = "R<hostIP>:<hostPort>"` и `hostpw`/`slavepw`, полученные от хоста.
   Игра гостя подключается к relay хоста как WServer-клиент.

Игровой трафик идёт гость → машина хоста напрямую. Ни Shiro, ни Steam в пути нет.

### Код подключения

Crockford base32, как `UnifiedCode.cs` в PhoenixPoint\Multiplayer2:
`[flags:1][ipv4:4][port:2]` → 12 символов + 1 контрольный. Алфавит без `I`, `L`, `O`, `U`.

## Установка (один раз, автоматически)

1. Генерируем свой CA и leaf-сертификат с SAN `master.shirogames.com`, CA кладём
   в хранилище `LocalMachine\Root` (нужен админ).
2. В `%SystemRoot%\System32\drivers\etc\hosts` добавляем `127.0.0.1 master.shirogames.com`
   и `127.0.0.1 master2.shirogames.com` в помеченном блоке (чтобы чисто снимать).
3. `wartales-mp.exe` прописывается в параметры запуска Steam: он поднимает master+relay,
   запускает `Wartales.exe`, по выходу игры гасит себя.

Обе правки обратимы командой `wartales-mp.exe uninstall`.

## Границы применимости

Транспорт игры — TCP, поэтому UDP hole punch неприменим. Если у хоста CGNAT и UPnP выключен,
прямое соединение не построится — это ограничение NAT, не кода. Гостю проброс не нужен.

## Контракт мастера

Полная схема команд и ответов, снятая из байткода: `../decomp/SERVER-CONTRACT.md`.
Декомпилированные исходники mpman: `../decomp/mpman/`.
