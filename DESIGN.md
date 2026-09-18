# wartales-mp — замена сетевого коннекта Wartales

Цель: коннект между игроками идёт напрямую, минуя инфраструктуру Shiro, а если у хоста нет
доступного извне порта — через современный релей Valve (Steam Datagram Relay). Лестница
транспортов ровно из двух ступеней: **direct → SDR**. Легаси-путь Steam P2P
(`ISteamNetworking`, тот самый релей, который и ломается) убран по построению.
Игра не патчится. Паки и `steam.hdll` не трогаем; байткод (`hlboot.dat`) на диске тоже, но
в его копии под `%LOCALAPPDATA%` правятся три байта (`internal/hlpatch`): два выключают
фатальный таймаут клиента, третий пускает коды длиннее 5 символов через «join by code».

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
   Приглашение через Steam на этом же протоколе: «пригласить друзей» шлёт
   `lobby/initInvite {id}` → строка; игра создаёт Steam-лобби (friends-only, 256 мест),
   пишет строку в его данные под ключом `invite` и открывает оверлей. У друга «Join Game»
   → `+connect_lobby` → `getRawData("invite")` → `lobby/infoInvite {invite}` → LobbyInfo →
   `lobby/join {id}`. Наш мастер отвечает на `initInvite` кодом подключения, а `infoInvite`
   разрешает его как введённый код (или как локальный id лобби).
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

Crockford base32, как `UnifiedCode.cs` в PhoenixPoint\Multiplayer2. Direct:
`[flags:1][ipv4:4][port:2]` → 12 символов + 1 контрольный; SDR: `[flags:1][accountID:4][key:4]`
→ 15 + 1; оба маршрута → 24 + 1 (см. «Код подключения: три формата» ниже). Алфавит без
`I`, `L`, `O`, `U`.

### Лестница транспортов: direct → SDR

Переключатель — uid участников лобби. `Lobby.isSteamOnly@24596` возвращает true, если uid
КАЖДОГО участника начинается с `S` (платформенный символ Steam). Тогда
`Lobby.setupPlatform@24597` вообще не шлёт `instance/get`: хост отдаёт гостям свой
`getUser().id` через hxbit-сообщение лобби, и игра идёт по своему Steam-пути
(`mpman.net.SteamService`, `steam_send_p2p_packet` и т. д.). Любой не-`S` uid в лобби —
и игра спрашивает `instance/get`, то есть наш relay. Кто рендерит uid, тот и выбирает
транспорт; выбирает мастер, один раз, при `lobby/create` (`chooseTransport@internal/master/transport.go`):

| Ступень | Когда | Что уходит на провод |
| --- | --- | --- |
| **direct** (наш relay) | `nat.Endpoint.Verified` — на публичный порт в этом запуске УЖЕ приходило соединение из интернета (`relay.OnInbound` → `nat.Mapper.MarkInbound`, только публичный источник считается). UPnP-маппинг + адрес от STUN (`Reachable`) — лишь подсказка: на живой машине это дало «reachable», а порт снаружи был refused | Session-id `X<hex>` для всех: `uid.Mint` детерминированно отображает `S<steamid>` из `user/login` в `X…`, тот же игрок — тот же id на обоих концах proxy-link |
| **SDR** (релей Valve), целиком | endpoint не подтверждён (не было входящих) или его нет вовсе — «десять роутеров вглубь», ни порта, ни адреса | настоящие Steam-id игроков ровно как их сообщила игра (`S` + 16 hex, `UserID.hx:29/113`) — игра выводит из них SteamID64 пира; шим переносит легаси-вызовы на `ISteamNetworkingMessages`; код подключения несёт SteamID хоста, и proxy-link (лобби-фаза) тоже едет по SDR через мост (см. ниже). Код при этом ВСЁ РАВНО несёт и endpoint-подсказку первым маршрутом: гость, который до неё дотянется, придёт по TCP — и этим подтвердит endpoint для следующего лобби |
| direct без подтверждения | SDR в этом процессе невозможен (`unavailable`), а подсказка есть | единственный оставшийся маршрут; в лог — `no SDR, falling back to the unverified endpoint` |
| — (отказ) | ни endpoint, ни SDR | `lobby/create` отвечает `err "Cannot host: no usable transport: …"` с обеими причинами. «Ещё неизвестно» (шим ждёт Steam API) — НЕ отказ: лобби создаётся, а `lobby/makeShortCode` отвечает `Steam relay not ready yet (…); ask for the code again in a few seconds`, пока мост не поднимется |

Принудительно: `wartales-mp run -transport direct|sdr` (`sdr` при заведомо неработающем SDR
всё равно отказ). Решение и причина пишутся в `wartales-mp.log`
(`lobby L… game transport: SDR (endpoint 192.168.1.5:14250 not internet-reachable: …; SDR ready)`).

Как решение доходит до гостя: гость проксирует все `lobby/*` мастеру хоста, и все id, которые
видит игра гостя (`lobby/join` → `LobbyInfo.users[].id`, `owner`, пуши), отрендерены мастером
хоста. Отдельного поля не нужно — вердикт едет внутри id; мастер гостя лишь логирует его по
`owner` ответа (`joined lobby L…; the host's master chose SDR`). Гость в `link/hello` передаёт
и минтед-id, и свой Steam-id (`link.User.Steam`); `link.Serve` принимает Steam-id только
правильной формы (`uid.IsSteam`), иначе гость остаётся на `X`-id — лобби перестаёт быть
Steam-only, что логируется предупреждением (игра тогда спросит `instance/get`).

Рендер id — `lobby.idOf(peer)`; все места, где id уходит наружу или сравнивается
(`LobbyInfo`, `owner`, `lobby/join|leave|setUserData|chat|transfer`, `peerGone`, `broadcast`),
идут через него. Регрессии закрыты тестами `TestEmittedUIDsAreNotSteamShaped` (direct: ни одного
`S`), `TestSDRLobbyEmitsRealSteamIDs` (SDR: только настоящие `S`),
`TestSDRLobbyWithoutSteamIDFallsBackToSession`, `TestNoTransportIsRefused`, `TestChooseTransport`.

### SDR-мост: proxy-link без порта

SDR живёт в процессе игры (шим, через её же Steam-клиент), мастер — в хелпере. Между ними
мост (`shim/proxy/bridge.c` ↔ `internal/sdrbridge`): шим при READY поднимает **loopback-TCP**
слушатель на `127.0.0.1:<порт от ОС>` и публикует его в `sdr.status`:
`ok bridge=127.0.0.1:52669 token=<32 hex>`. Loopback-TCP, а не именованный канал: на стороне
Go это обычный `net.Conn` (дедлайны, `Close` снимает `Read`, без overlapped I/O), в игре уже есть
winsock, bind только на 127.0.0.1 не вызывает запрос файрвола, порт от ОС не конфликтует.
Хелпер следит за файлом (раз в 500 мс, всё время работы — Steam API поднимается позже него),
подключается, первым кадром шлёт токен — без него соединение молча закрывается.

Кадр на сокете в обе стороны: `[type:u8][peer SteamID64:u64 LE][len:u32 LE][payload]`;
`0 AUTH` (токен), `1 SEND` (хелпер→шим: доставить пиру), `2 RECV` (шим→хелпер: пришло от пира),
`3 ERR` (шим→хелпер: Steam отказал в отправке, текст с EResult). Шим шлёт `SendMessageToUser`
с `Reliable|NoNagle|AutoRestartBrokenSession` и раз в 20 мс вычерпывает
`ReceiveMessagesOnChannel` на **канале 100** — выделенном: `SteamService` игры использует только
канал 0, шим обслуживает игре каналы 0..7, а `pump` игры тянет только запрошенный канал, так что
трафик моста в очереди игры не попадает и наоборот. Поверх кадров — потоки по одному на пира
(`sdrbridge.peerConn`, `net.Conn`): SDR reliable и упорядочен per-peer/per-channel, поэтому
payload'ы просто конкатенируются; первый байт payload — `1 data` / `2 fin`. На них без изменений
работает `link` (`link.DialConn` / `link.Serve`): у гостя `lobbyResolveShortCode` → `Bridge.Dial(steamID)`,
у хоста `Bridge.OnPeer` → `ServeSDRLink`.

Защита от чужого пира: SteamID хоста публичен, а поток на канал 100 может открыть кто угодно.
Поэтому `link/hello` по SDR обязан нести 32-битный `key` из кода подключения (случайный на запуск
хелпера, `Options.LinkKey`); `ServeSDRLink` сверяет и отвечает `err "Invalid join code"` на сам
hello (uid 1), поток закрывается, ни одна команда не обслуживается. Поток от пира, не
представившегося, дальше `link.Serve` не проходит; переполнение очереди пира (256 кадров)
закрывает поток. TCP-proxy-link (direct) ключа не требует — как и раньше, доступность порта
и есть доказательство.

### Код подключения: три формата, один код — оба маршрута

Crockford base32 (без `I`, `L`, `O`, `U`), последний символ — контрольный (сумма тел mod 32),
биты 7 и 6 байта флагов говорят, что внутри; различаются длиной:

| Формат | Payload | Длина | Пример |
| --- | --- | --- | --- |
| endpoint (direct) | `[flags:1, bit7=0][ipv4:4][port:2]` | 12 + 1 = 13 | `01C0SJ036YN0M` = 88.12.200.3:14250 — как в первом релизе, старые коды декодируются |
| steam (SDR) | `[flags:1, bit7=1][accountID:4 BE][key:4 BE]` | 15 + 1 = 16 | `G00BRRAEVTPVXVRS` = account 12345678 (SteamID64 `0x0110000100BC614E`), key `deadbeef` |
| combined (оба) | `[flags:1, bit7=1, bit6=1][ipv4:4][port:2][accountID:4 BE][key:4 BE]` | 24 + 1 = 25 | `R0PSMP226YN01F319VFAVFQFF` = 45.154.88.66:14250, затем account 12345678, key `deadbeef` |

Хост выдаёт combined, когда есть и endpoint (любой IPv4, подтверждённый или нет — даже LAN), и
SDR-маршрут (мост поднят); только один из них — соответствующий одиночный формат. Endpoint
пере-резолвится для каждого кода (`nat.Mapper.MaxAge`, 90 с): WAN-адрес меняется между лобби
(на живой машине чередовались .64/.66), UPnP-шлюз отвечает через раз; смена адреса сбрасывает
`Verified`. Подтверждённость endpoint'а НИКОГДА не убирает SDR-маршрут из кода.

### Каскад на стороне гостя

`lobbyResolveShortCode` → `cascade`: маршруты пробуются по порядку, и маршрут засчитан только
когда мастер хоста реально ответил на `lobby/resolveShortCode` через него:

1. **direct**: TCP-connect с таймаутом `DefaultDirectTimeout` = 3 с (refused — мгновенно;
   молча выброшенный SYN — типичный файрвол — не должен держать игрока: 3 с — один ретрансмит
   после первого SYN в Windows), затем `link/hello` с ключом из кода и пробная команда с
   таймаутом 5 с. `ServeLink` хоста сверяет ключ, если гость его предъявил (старые
   endpoint-коды без ключа принимаются как раньше): адрес мог достаться чужому хелперу —
   его мастер ответит `Invalid join code`, линк сбрасывается.
2. **SDR**: `Bridge.Dial(steamID)`, `hello` с ключом, команды с таймаутом 8 с.

Худший случай ≈ 3 + 5 + 8 = 16 с < 20 с таймаута команды у игры. Каждый исход — в
`wartales-mp.log` (`route direct 45.154.88.66:14250 failed: …`, `falling back to SDR …`,
`route SDR to SteamID … WORKS`); игрок видит только лишнюю паузу. Если не сработало ничего —
`Cannot reach the host: direct …: <причина>; SDR to SteamID …: <причина>`. Тесты:
`TestCascadeDirectWins`, `TestCascadeDirectRefusedFallsBackToSDR`,
`TestCascadeDirectTimesOutFallsBackToSDR`, `TestCascadeWrongHostBehindTheEndpoint`,
`TestCascadeBothFail`, `TestIssuedCodeCarriesBothRoutes`, `TestCombinedRoundTrip`.

### Windows Firewall — решение

У хелпера нет входящего правила (на живой машине `Get-NetFirewallApplicationFilter` его не
нашёл), значит direct не заработает даже при верном маппинге. Хелпер запускается скрытым
изнутри игры и НЕ запрашивает повышение: UAC-окно без своего окна за полноэкранной игрой
закроют, и мод молча сломается. Поэтому: при старте `firewall.Check` (`netsh advfirewall
firewall show rule name=wartales-mp dir=in`) пишет в лог `WARNING: firewall: no inbound rule
"wartales-mp"; the direct route … cannot work until it exists` с подсказкой запустить
`wartales-mp firewall`; direct-маршрут остаётся «подсказкой без подтверждения», SDR правила
не требует. Одно
ручное действие, только если хочется direct: `wartales-mp firewall` из консоли администратора
(`netsh … add rule … program=<exe> localport=14250`), `wartales-mp firewall check` — проверить.

SteamID64 восстанавливается из 32-битного account id: у любого игрового аккаунта старшие
32 бита — `0x01100001` (universe Public, type Individual, instance Desktop). Мастер получает
SteamID64 хоста из его же `S`-uid (`uid.SteamID64`: 8 байт hex little-endian, старший dword
xor `0x1100001`). `code.DecodeAny` возвращает `Endpoint` и/или `Steam`; `wartales-mp code
decode` печатает, что есть.

Порядок запуска: хелпер стартует раньше, чем Steam API игры готов. Пока шим не написал
`ok bridge=…`, `lobby/create` проходит (SDR = оптимистичный выбор), а `lobby/makeShortCode` и
`lobby/resolveShortCode` по steam-коду отвечают ошибкой с причиной и просьбой повторить через
несколько секунд (`Steam relay not ready yet (SDR still initialising: …)`), и это же пишется в
`wartales-mp.log`. Очереди нет: игра показывает текст, игрок жмёт ещё раз. Если шим написал
`unavailable`, `lobby/create` без публичного адреса отказывает сразу (см. таблицу).
Тесты: `TestJoinOverSDR` (два мастера через поддельный SDR-коммутатор `sdrbridgetest`: код с
SteamID, `resolveShortCode` → `join` → пуши `lobby/join`/`lobby/chat` через мост, чужой ключ —
отказ), `TestSDRJoinBeforeTheBridgeIsReady`, `sdrbridge.TestStreamsOverTheSwitch`.

### SDR в шиме: легаси-нативы переведены на `ISteamNetworkingMessages`

`steam.hdll` (hlsteam) реализует Steam-транспорт игры на **устаревшем** `ISteamNetworking`
(`SteamNetworking006`): `steam_send_p2p_packet`, `steam_read_p2p_packet`,
`steam_is_p2p_packet_available`, `steam_accept_p2p_session`, `steam_close_p2p_session`,
`steam_get_p2p_session_data`. Байткод резолвит их через `hlp_<name>(&sign)`, который
возвращает адрес тела функции; шим (`shim/proxy/sdr.c`) хукает ровно этот адрес через MinHook
(проверено: он совпадает с экспортом `steam_<name>`) и НИКОГДА не вызывает оригинал —
трамплин не используется. Реализация поверх flat-API `steam_api64.dll` (все экспорты
проверены в поставляемой DLL, у которой SmokeAPI форвардит их в `steam_api64_o.dll`):

| Легаси-натив | SDR |
| --- | --- |
| `send_p2p_packet(uid, data, len, type, ch)` | `SendMessageToUser(identity(SteamID64 из 8 байт uid), data, len, flags, ch)`; `type` 0→`Unreliable`, 1→`Unreliable\|NoDelay\|NoNagle`, 2→`Reliable\|NoNagle`, 3→`Reliable`; всегда `+AutoRestartBrokenSession`; `true` ⇔ `k_EResultOK` |
| `is_p2p_packet_available(&size, ch)` | докачивает `ReceiveMessagesOnChannel(ch)` в свою FIFO канала и сообщает размер ГОЛОВЫ очереди |
| `read_p2p_packet(buf, max, &len, ch)` | снимает голову очереди, копирует `min(size, max)` (усечение как в `ReadP2PPacket`, хвост пропадает вместе с пакетом), `len` = скопировано, возвращает SteamID64 отправителя как свежие 8 байт (`hl_copy_bytes`), `Release` сообщения |
| `accept_p2p_session` / `close_p2p_session` | `AcceptSessionWithUser` / `CloseSessionWithUser`; close ещё выкидывает из очередей всё от этого пира |
| `get_p2p_session_data` | `null` («сессии нет»); игра его не вызывает |

Входящие сессии: игра принимает их по легаси-колбэку `P2PSessionRequest_t`, который у нового
интерфейса не бывает, — поэтому шим регистрирует
`SetGlobalCallback_MessagesSessionRequest` и принимает сам (и логирует
`MessagesSessionFailed`). `InitRelayNetworkAccess` вызывается заранее: отдельный поток ждёт
`SteamAPI_GetHSteamUser() != 0` и инициализирует транспорт до первого пакета.

Fail closed, без отката: нет `steam_api64.dll`, нет экспорта, `SteamNetworkingMessages002`
не отдался — нативы ведут себя как транспорт, который не соединяется (send → false, available →
false, read → null), причина — в `shim.log` (`sdr: UNAVAILABLE, transport disabled: …`) и в
`%LOCALAPPDATA%\wartales-mp\sdr.status` (`ok` / `pending <why>` / `unavailable <why>`), который
мастер читает при каждом `lobby/create`. Игра показывает ошибку соединения, а не тихо едет на
старом релее. Доказательство без Steam — `shim\check.ps1` (см. ниже).

## Установка: ровно один новый файл в папке игры

В папке игры ничего не переименовывается, не заменяется и не правится. Мод — это
единственный новый файл `winmm.dll`, который кладётся в папку игры; игра сама грузит
его при запуске. Ни `hosts`, ни сертификатов в системе, ни параметров запуска Steam,
ни правки реестра, ни прав администратора, ни инжекта снаружи.

### Почему это работает

Windows ищет неизвестную (не-KnownDLL) библиотеку в каталоге приложения раньше, чем
в `System32`. Игровые бинарники статически импортируют системные DLL, которых в папке
игры нет, — значит наш файл под таким именем перехватит загрузку первым.

| Кандидат | Кто импортирует статически | Экспортов | Когда грузится |
| --- | --- | --- | --- |
| `WINMM.dll` | `libhl.dll` (её статически импортирует `Wartales.exe`) | 180 именованных | на инициализации процесса, раньше всего |
| `VERSION.dll` | `SDL3.dll` | 34 | чуть позже |

Выбран `WINMM.dll`. Он грузится раньше всех и, как собственная зависимость `libhl.dll`,
гарантирует, что к моменту нашего `DllMain` `libhl.dll` уже отображена в память — а именно
её мы патчим. Больший список экспортов ничего не стоит: форвардеры генерируются
`tools/gendef` автоматически. Проверено: ни `WINMM`, ни `VERSION` не входят в KnownDLLs
(`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\KnownDLLs`), поэтому порядок поиска
по каталогу приложения применяется к обоим.

### Что делает `winmm.dll`

| Задача | Как |
| --- | --- |
| форвардинг | все именованные экспорты уходят в настоящую `System32\winmm.dll` (грузим по полному пути через `GetSystemDirectoryW`+`LoadLibraryW`, не по голому имени — иначе рекурсия в себя). Строку-форвардер PE использовать нельзя (она не может называть собственный модуль), поэтому `tools/gendef` из таблицы экспорта настоящей `winmm.dll` генерирует по тонкому thunk'у на экспорт: `jmp` через указатель, который заполняется `GetProcAddress` при загрузке |
| `hl_host_resolve` (`libhl.dll`) | `master*.shirogames.com` → `127.0.0.1` (`0x0100007F`), остальное — насквозь |
| `ssl_conf_set_ca` (`ssl.hdll`) | наш локальный CA добавляется в цепочку через оригинальный `ssl_cert_add_pem`, дальше вызов идёт насквозь; проверку сертификата НЕ отключаем |
| шесть P2P-нативов `steam.hdll` | переведены на `ISteamNetworkingMessages` (SDR), см. выше; оригиналы не вызываются никогда |
| SDR-мост | loopback-TCP слушатель для хелпера (`bridge.c`), канал 100, порт и токен в `sdr.status` |
| `CreateFileW`/`CreateFileA` (`kernelbase.dll`) | открытие `hlboot.dat` на чтение подменяется на `%LOCALAPPDATA%\wartales-mp\hlboot.dat` — копию оригинала с 2 патченными байтами (файл в папке игры не трогаем). Копия при каждом открытии сверяется побайтно с «оригинал + патч» и перегенерируется, если отличается или отсутствует; если сигнатура в оригинале не нашлась ровно один раз (обновление игры) — игре отдаётся её собственный файл. Таблица — `internal/hlpatch`, генерится в `shim/proxy/hlpatch.h`. Обе сигнатуры — проверка таймаута клиента в `hxbit.NetworkHost.flush@3969`; после патча сравнение `clientTimeout !< clientTimeout` всегда истинно, и `c.timeout()` не вызывается никогда. Без этого игра падает `Null access` (`mpman/net/Client.hx:13`) примерно через минуту простоя в лобби: путь таймаута в этой сборке фатален для любой роли (см. `decomp/SERVER-CONTRACT.md` §7.3–7.4) |
| лог | `%LOCALAPPDATA%\wartales-mp\shim.log`: attach, каждый хук (адрес или причина отказа), каждое открытие байткода (путь, размер, fnv1a, смещения патчей, копия reused/written/fallback), запуск ядра, состояние SDR (READY / причина отказа, первый пакет, сессии, счётчики при close). Каждая строка — свой open/append/close |
| запуск ядра | извлекает вшитый `wartales-mp.exe` в `%LOCALAPPDATA%\wartales-mp\` (только если файла нет или сборка отличается по хэшу) и стартует его скрыто; тот сам следит за PID игры и выходит вместе с ней |

Байткод резолвит нативы через указатели `hlp_<name>` при загрузке модуля, а не через IAT,
поэтому патч IAT эти две функции не поймал бы — нужен инлайн-хук по телу функции. Хуки ставит
MinHook (BSD-2, вендорится в `shim/minhook`): `MH_CreateHook` даёт трамплин для вызова оригинала.
`libhl.dll` уже отображена — её хук ставится сразу; `ssl.hdll` и `steam.hdll` грузятся лениво,
поэтому фоновый поток ждёт их появления опросом `GetModuleHandleW` (а не хукает нагруженный
`LoadLibraryExW` под loader lock) — `conf_set_ca` вызывается один раз и поздно, при первой
настройке TLS, а P2P-нативы — только при старте игры из лобби, так что опрос успевает с запасом. Хук `CreateFileW` — исключение: `hlboot.dat` открывается из `main()` игры до
того, как успеет стартовать фоновый поток, поэтому он ставится прямо из `DllMain` (только
`kernelbase`, она отображена и инициализирована задолго до нас). Именно `kernelbase`, а не
`kernel32`: загрузчик HashLink (`src/main.c`, `load_code`) читает файл через `_wfopen`+`fread` из
`ucrtbase.dll`, а та импортирует `CreateFileW`/`ReadFile` из `api-ms-win-core-file-l1-1-0` →
`kernelbase.dll`, минуя экспорт-заглушки `kernel32` (первая версия хукала `kernel32!ReadFile` и
поэтому никогда не срабатывала). Заглушки `kernel32` сами прыгают в `kernelbase`, так что хук
там ловит всех. Проверка без запуска игры: `shim\check.ps1` грузит собранную `winmm.dll` в
тестовый процесс, открывает настоящий `hlboot.dat` через `_wfopen`/`fread` и `CreateFileW`/`ReadFile`
и сверяет результат с ожидаемым образом. Она же доказывает SDR-транспорт без Steam: в процесс
заранее грузятся три подделки из `shim/check` — `libhl.dll` (только `hl_copy_bytes`),
`steam.hdll` (легаси-нативы с настоящими `hlp_`-резолверами, каждый вызов считается — счётчик
обязан остаться 0) и, в режиме `sdr`, `steam_api64.dll` (in-process loopback
`ISteamNetworkingMessages`: отправленное пиру X ставится в очередь как пришедшее от X). Нативы
вызываются через `hlp_<name>`, как это делает байткод, и проверяются: флаги по типу отправки,
границы пакетов, усечение, независимые очереди каналов, SteamID отправителя, автоприём сессии,
close выкидывает очередь пира, каждое сообщение отпущено `Release`. Затем мост: из `sdr.status`
берутся порт и токен, чужой токен → соединение закрыто, свой → `SEND` пиру возвращается `RECV`
от него (loopback), отправка ушла на канал 100 с `Reliable|NoNagle|AutoRestart`, инъекция от
другого пира на канал 100 приходит с его SteamID, канал 0 игры её не видит, отказ Steam приходит
`ERR`-кадром с EResult. Режим `nosdr` (без `steam_api64.dll` вообще) проверяет fail closed: send
false, available false, read null, причина в `shim.log` и `sdr.status` (и никакого `bridge=`),
легаси по-прежнему не вызван.

### Сборка

`shim\build.ps1`. Порядок разрешает то, что exe вшит в DLL, поэтому exe должен существовать
раньше:

| Шаг | Результат |
| --- | --- |
| `gendef` читает `System32\winmm.dll` | `winmm_stubs.c` + `winmm.def` (thunk'и + алиасящий `.def`) |
| `hlpatchgen` читает `internal/hlpatch` | `shim/proxy/hlpatch.h` (таблица байт-патчей) |
| `go build` | `wartales-mp.exe` — финальное ядро |
| `objcopy -I binary -O pe-x86-64` оборачивает exe | `embed.o` — линкуемый блоб |
| `gcc -shared` линкует `proxy.c` + `sdr.c` + `bridge.c` + thunk'и + MinHook + `embed.o` + `.def` (`-lws2_32`) | `winmm.dll` |

Единственный файл для папки игры — `dist\winmm.dll`. Обновление игры в Steam его не тронет
(это новый файл, а не подмена); чтобы выключить мод, файл просто удаляют — игра возвращается
к ванильной сети.

#### Релизная сборка: `-Release` (стрип + UPX)

По умолчанию `shim\build.ps1` даёт нестрипнутый, неупакованный DLL — для отладки. Ассет,
который выкладывается на GitHub, собирается `shim\build.ps1 -Release`:

- **стрип символов**: `-ldflags "-s -w"` для Go-exe, `-s -Wl,--strip-all` для линка DLL;
- **UPX `--best --lzma`** на обеих ступенях: вшитый `wartales-mp.exe` пакуется *до* обёртки в
  `embed.o` (так копия, которую шим извлекает в `%LOCALAPPDATA%` и запускает, — тоже упакована),
  затем пакуется финальный `winmm.dll`. Флаги подобраны безопасными для PE-DLL: не трогают
  таблицу экспорта и base-релокации (никакого `--strip-relocs`), а форвардинг именованных
  экспортов и релокации DLL нужны для работы дроп-ина.
- **размеры** (замерено, UPX 5.2.1): exe 8.05 МБ → 2.50 МБ (31 %); финальный DLL — с вшитым уже
  упакованным exe — почти не жмётся на последнем шаге (2.568 МБ → 2.558 МБ), но весь артефакт
  падает с 11.77 МБ (неупакованная сборка) до 2.56 МБ.

**Упаковка проверена, а не предположена.** После `-Release`: `tools/gendef -in dist\winmm.dll
-list` даёт все 180 имён с исходными ординалами, байт-в-байт совпадая с неупакованной сборкой;
`shim\check.ps1` проходит в обоих режимах против упакованного DLL (форвардеры, хук `CreateFileW`,
MinHook, SDR-нативы и мост — всё работает, `forwards: 180 exports bound, 0 missing`); упакованный
вшитый exe запускается (`wartales-mp code decode` даёт верный адрес). Если бы UPX что-то ломал
(форвард-thunk'и, способность MinHook патчить, хук `CreateFileW`, вшитый ресурс) — релиз остался
бы неупакованным; на этой сборке ничего не сломалось.

**Антивирусы.** UPX-упаковка заметно повышает ложные срабатывания антивирусов, а этот DLL
инжектится в процесс игры — для эвристик это хуже вдвойне. Релизный ассет упакован ради размера;
неупакованную (и нестрипнутую) сборку с тем же поведением даёт `shim\build.ps1` без `-Release`.
Компромисс сознательный: размер/детект против удобства скачивания.

Для игрока: копирует `winmm.dll` в папку игры, жмёт «Играть» в Steam как обычно, создаёт лобби
в ванильном интерфейсе, игра показывает код, код кидается другу — тот вводит его в игре и
подключается. Запасного пути на старом Steam-релее нет: либо direct, либо SDR.

## Границы применимости

Транспорт игры на direct — TCP, поэтому UDP hole punch неприменим. Пока на порт хоста не
приходило соединение из интернета (нет правила файрвола, маппинг не сработал, CGNAT), лобби
идёт на SDR целиком: код несёт endpoint-подсказку и SteamID, гость пробует direct 3 с и
переходит на SDR, лобби-фаза идёт через SDR-мост, игровая — через SDR-транспорт шима; хосту
не нужен ни порт, ни адрес. Первый гость, дошедший по TCP, подтверждает endpoint — следующее
лобби уже direct. Гостю проброс не нужен ни на одной ступени. Что остаётся условием SDR: у обоих запущен
Steam-клиент с работающим `SteamNetworkingMessages002` и доступом к релеям Valve (если релеи
Valve недоступны из сети игрока, SDR не поможет — тогда нужен direct). Сквозной SDR-сеанс двух
игроков со Steam ещё не прогонялся: проверены семантика нативов и мост на loopback-подделке,
два мастера через поддельный коммутатор и выбор транспорта, а не живой релей Valve.

## Контракт мастера

Полная схема команд и ответов, снятая из байткода: `../decomp/SERVER-CONTRACT.md`.
Декомпилированные исходники mpman: `../decomp/mpman/`.
