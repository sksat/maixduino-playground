# maixduino-test

**Sipeed Maixduino**（Kendryte K210）でベアメタル **Rust**。最初の「動いた」を2つ同時にやる:

ペリフェラルを1個ずつ触っていく実験リポジトリ。`src/main.rs` がその時の題材
（過去のは git 履歴に。シリアル出力は UARTHS、基本 115200／現カメラデモは 1.5Mbaud）。

- **カメラ (OV2640/DVP) — 320×240 RGB565 撮影**（コミット `f107bf9` + [src/dvp.rs](src/dvp.rs)）:
  RGB565 フレームを DVP（自前 AXI マスタ）で SRAM に取り込み → シリアルにダンプ → ホスト `uv run python`
  で PNG 化（`captures/`、未コミット）。実機で**クリーンな実写真**（何が写ってるか分かる、確実）。
  ハマり所: ① **カメラ FFC の逆挿し**で全 SCCB が 0xff（未接続と同症状）、② **PCLK が速すぎて水平に化ける**
  → XCLK 分周を 3→7 に下げて解決。DVP には**キャッシュ有りアドレス**を渡し CPU は**無しエイリアス
  (0x4000_0000)**で読む。DVP/SCCB ドライバは [laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff)
  から移植。
- **カメラ映像を WiFi で配信する Web サーバ (step 8 = ゴール・ライブ版、WiFi を UART に逃がして高速化)**
  （現 `src/main.rs` + [src/dvp.rs](src/dvp.rs) + [src/uart_wifi.rs](src/uart_wifi.rs) +
  [esp32-modem-ninafw/](esp32-modem-ninafw/)）: ブラウザでアクセスすると**カメラのライブ画像 + 解像度切替ボタン**。
  **解像度を Web から選択可能**（`/cam.bmp?r=0/1/2` → QQVGA 160×120 / QVGA 320×240 / VGA 640×480）で、
  リクエストの `r=` を見て**OV2640 をその場で再設定**（サイズが変わった時だけ ~185 SCCB 書込＋ウォームアップ、
  同じ解像度の連写は撮影のみ）。毎リクエスト撮り直してフル BMP をライブ配信し、実機で全解像度フル完走を確認:
  **QQVGA(57KB)≒0.27s / QVGA(230KB)≒1.0s / VGA(922KB)≒4.3s（K210 実測の配信時間、3Mbaud）**。旧 SPI 版（下の step 7）は
  160×120 で ~6–8s/枚だったので、QQVGA 同士で 20倍速以上・解像度は VGA まで。**UART リンクが律速**で、内訳は
  実測でも**UART 転送が約70〜75%（バイト数 ÷ 3Mbaud）＋チャンク往復オーバーヘッド約25〜30%**。どちらもバイト数に
  比例するので**配信時間 ≒ 画像サイズに線形**（57:230:922KB ≒ 274:1020:4275ms）。リンクは **3 Mbaud**
  （K210 分周 195MHz/48=4.0625 ちょうど）まで上げた。
  フレームバッファは VGA 分（614KB）を確保し小解像度は先頭を使う。解像度切替時は OV2640 再 init で AE/AWB が
  リセットされ収束前は緑かぶり（ベイヤの緑2倍が補正前は勝つ）が出るので、`configure_res` のウォームアップを
  **枚数固定でなく実時間 ~2秒**捨て撮りして収束させ、切替直後の一枚目から色が合うようにした（同解像度の連写には
  かからない）。撮影画像には DVP 由来の横縞＋ごま塩ノイズ（この基板の DVP 信号品質の限界、step「UXGA JPEG」参照）が
  乗る（転送はバイト正確で、ノイズはあくまで撮影側）。これを**Web からトグルできるオンチップ・デノイズ**で軽減
  （`&d=1`、ページに ON/OFF ボタン）: ①**3枚 temporal median**でごま塩除去（DVP のバイト誤りはフレーム毎にランダムなので
  3枚の中央値で正値が残る。CAP[0..3] の3バッファ＝VGA で 1.84MB）、②**per-row destripe**で横縞低減（G(6bit)を luma 代用に
  各行の明度を全体平均へ寄せ、色は保つ）。実測 QVGA で**ごま塩 1024→263px・行縞 std 10.5→7.7**、デノイズ計算＋撮影2枚増は
  UART 転送に対して誤差なのでフレーム時間はほぼ不変。
  さらに **JPEG モード**（`&j=1`、ページに JPEG ボタン）も選択可: OV2640 の**ハードJPEG**を DVP で取り込み（バイトは
  32bit ワード内ビッグエンディアンなので `swap_bytes` で連続化、`FFD8`..`FFD9` を走査）、QVGA で **~8KB を ~0.5s** 配信。
  ただし**この基板では JPEG は癖が強い**: UART 律速の「バイト削減」狙いで試したが、**DVP のバイト誤り＋OV2640 が
  リスタートマーカーを出さない**ため、(a) UXGA(105KB)は最初の誤りで以降全滅（上端 数%のみ）、(b) QVGA(~8KB)は Huffman は
  100% 通る（大崩壊は回避）が、誤りが **JPEG の DC 係数**（前ブロックからの差分・マーカー無しで再同期不可）に当たると
  **そのフレーム全体が一色に転ぶ**＝視認可は ~30%（実測 20枚）。同じ DVP 誤りでも RGB は1画素のごま塩（局所・除去可）で
  済むのに JPEG は増幅する、という対比。**使える時は速くて小さい**ので任意モードとして残置（出力サイズだけ書くと
  scaler が壊れるので ArduCAM 320x240_JPEG の窓+scaler+PCLK を一括適用）。
  **BMP 最適化＝RGB565 直送モード**（`&e=1`、ページに RGB565 トグル）: K210 が RGB565→24bit に膨らませてから UART に
  流していた（1.5倍に水増し）のをやめ、**RGB565(2B/px) のまま送って ESP32 側で BGR24 展開**（新コマンド `B`）。律速の
  UART が **33% 減**でロスレス（ブラウザには同じ 24bit BMP）。実測 QVGA **1000→790ms（−21%）**、VGA **4275→3447ms**、
  UART バイトはきっちり 2/3（230541→153741 / 921654→614541）。デノイズとも併用可。**UI**: 全モードをページのボタンで
  切替（再フラッシュ不要）、アクティブを緑ハイライト＋現在状態表示、**JPEG 中は Denoise/RGB565 を無効化**（BMP 専用）。
  **効いた一手＝WiFi を SPI0 から UART に逃がした**こと。旧版の遅さは「カメラ(DVP)と WiFi(nina) が両方 SPI0 を
  使い、撮影が ESP32 のネットを壊す→撮影ごとに EN リセット＋再接続(~5s)」が原因だった。**ESP32 を UART
  modem 化**すれば WiFi はカメラと無関係な IO6/IO7 を通るので、撮影がネットを壊さない＝復旧ダンスが丸ごと消え、
  毎リクエスト新鮮なフレームを健全な接続で返せる。
  **ESP32 側の本当の難所**: 汎用 arduino-esp32 だと**この u-blox NINA-W102 モジュールで 802.11
  アソシエーションに失敗**（reason 2 AUTH_EXPIRE、association イベントが一度も来ない。idf 4.4 でも 5.5 でも、
  nina-fw 等価の最小接続—ch/BSSID 固定なし・threshold なし・PMF off・既定 protocol/country/TX—でも同症状）。
  PHY init data は両者とも既定の同じ blob なので、**犯人は idf バージョンの WiFi/PHY バイナリ blob** と切り分け
  （codex とも一致）。→ **アソシエーションが通る唯一の firmware＝nina-fw(idf 3.3) の WiFi スタックを流用し、
  トランスポートだけ SPI(SPIS)→UART0 に差し替え**たのが [esp32-modem-ninafw/](esp32-modem-ninafw/)。
  ビルドの罠: **idf v3.3 は gcc 5.2.0 ツールチェーンと対**で、新しい 8.2.0/esp-2019r2 を使うと newlib
  ヘッダ世代がズレて C++(cxx/asio)が `__result_use_check`/`_EXFUN` で全コケする（→ 5.2.0 を使う）。
  K210 側 [src/uart_wifi.rs](src/uart_wifi.rs) は UART1 を 16550 直叩き(3Mbaud)して modem プロトコル
  (`P`ing/`C`onnect/`L`isten/`A`ccept/`R`ecv/`S`end/`X`close)。**もう一つの罠＝nina-fw の `WiFiClient::write`
  は `lwip_send(MSG_DONTWAIT)` 一発で、送信バッファ満杯(`EWOULDBLOCK`)を致命扱いして**ソケットを閉じてしまう**。
  QQVGA(57KB)は遅くて踏まなかったが、QVGA(230KB)を速く流すと途中でソケットが死んで切れる。`select` で書込可能まで
  待って全バイト送る実装に直した（`wificlient-blocking-write.patch`）。これで**フロー制御は `client.write()` の
  ブロックそのもの**＝各 `S` 応答が律速になり、旧 nina(SPI)版の「無音ダンス」も不要。
  認証情報の扱いは step 4 と同じ（`wifi_creds.env` 非コミット→build.rs→`env!`、SSID/パスはシリアルに出さず IP のみ）。
- **【旧】カメラ映像 WiFi 配信 Web サーバ (step 7 = ゴール初達成、SPI 版)**（コミット `e6589e8`・タグ
  `nina-spi-camera-webserver`、[src/nina.rs](src/nina.rs) は資産として残置・`restore-nina-fw.sh` で SPI に戻せる）:
  カメラ(DVP)と WiFi(nina) が両方 SPI0 を使うため、**カメラ撮影が ESP32 のネットワークスタックを壊す**
  （SPIリンクは生きて GET_CONN_STATUS=3 なのに L2/TCP が死ぬ）。そこで ①今あるフレームを健全な接続で配信 →
  ②クライアントを閉じる → ③次フレームを撮影（ネットを壊す）→ ④EN リセット＋再接続で復旧、という構成で
  返信を常に健全な接続に乗せた（配信フレームは1リクエスト古い、~6–8s/枚）。再接続はフラついて conn=6/IP=0.0.0.0
  になることがあり **WL_CONNECTED まで最大8回リトライ**。**TCP フロー制御**: lwip 送信バッファは 4×MSS(5744B) で、
  SPI を叩き続けると ESP32 の WiFi タスクが餓えて 5744B ちょうどで切れる→**各 1024B 送信後に ~30ms の「無音」**
  を入れてドレインさせ 57KB を完走（codex に検証依頼）。[tools/bmp2png.py](tools/bmp2png.py) は numpy+zlib だけで
  BMP→PNG 拡大。この「撮影がネットを壊す」制約を UART 化で外したのが上の step 8。
- **ESP32（オンボード WiFi）— HTTP サーバ (step 6)**（コミット `444f036` + [src/nina.rs](src/nina.rs)）:
  WiFi 接続後、ポート80で待受し **ブラウザ/curl に HTML ページをサーブ**。実機で同一LANのホストから
  `curl http://192.168.0.7/` → `HTTP/1.1 200 OK`＋HTML を取得（`served sock 1 req 75B`＝実HTTPリクエストを
  パース）。nina サーバモデル: `GET_SOCKET`→`START_SERVER_TCP`(0x28) で待受、`AVAIL_DATA_TCP` を
  **2パラメータ `[listen_sock, accept=1]`** で呼ぶと**クライアントソケット番号**が返る（255=なし）→
  そのソケットで `GET_DATABUF`/`SEND_DATA`/`STOP`。レスポンスは `Content-Length` 付きでクリーンにクローズ。
  待受ソケットは**永続**（WiFiServer モデル）、`availServer` の accept フラグは **0**。**連続リクエストも安定**
  （curl ×4 すべて 200）。最大のハマり: **`SEND_DATA_TCP` は送信が16bit長だが応答は8bit長**(`waitResponseData8`)、
  `GET_DATABUF` は両方16bit。混同して応答を16bitで読むと検証失敗→リトライで同じ送信を連打→ESP32 が wedge→以降全滅。
  → 送受で長さ幅を独立指定（`request`=8/8, `request_wide`=16/16, `request_send`=16/8）。STOP 後は `wait_idle` で
  ESP32 が落ち着くのを待ってから次の accept。同一LANのホストから `curl http://192.168.0.7/` で検証。
- **ESP32（オンボード WiFi）— AP 接続 (step 4, nina-fw over 内蔵SPI0)**（コミット `9b4b84c` +
  [src/nina.rs](src/nina.rs)）: `SET_PASSPHRASE`(0x11) で SSID/パスを送り、`GET_CONN_STATUS`(0x20) を
  ポーリングして `WL_CONNECTED(3)` を待ち、`GET_IPADDR`(0x21) で**割り当て IP を取得**。
  **認証情報の扱い**: `wifi_creds.env`（**.gitignore・非コミット**）に置き、[build.rs](build.rs) が読んで
  `cargo:rustc-env` 経由でコンパイラへ、`src/main.rs` は `env!("WIFI_SSID")` で参照（[wifi_creds.env.example](wifi_creds.env.example)
  をコピーして記入）。**SSID/パスはシリアルに一切出さない**（出力は接続ステータスと IP のみ）。ハマり: `SET_PASSPHRASE` は
  ESP32 が WiFi 接続を開始するため**応答が遅い**→ハンドシェイクの READY 待ちを 100ms→1000ms に延長。
- **ESP32 — WiFi スキャン (step 3, nina-fw over 内蔵SPI0)**（コミット `02c22fc` + [src/nina.rs](src/nina.rs)）:
  ESP32 を nina-fw コマンドで叩いて**周辺の WiFi AP を列挙**（接続なし・認証情報不要）。
  `START_SCAN_NETWORKS`(0x36)→`SCAN_NETWORKS`(0x27) で SSID 一覧、`GET_IDX_RSSI`(0x32) で RSSI。実機で
  **周囲9個の AP を SSID＋RSSI(dBm) 付きで取得**。決め手は **bit-bang をやめて K210 の内蔵 SPI0(DesignWare SSI)**
  に置換したこと: 最適化なしビルドの bit-bang はクロックジッタで連続コマンドが化け・ESP32 が wedge していたが、
  ハードSPI は一定クロックで **GET_FW_VERSION ×8 が retries=0 で全成功**＝ジッタ消滅。CS/READY/EN だけ GPIO に
  残し（フレーム中 CS を保持）、SCLK/MOSI/MISO を SPI0 へ。k210-hal の SPI は transfer 系が未実装スタブなので
  **CTRLR0/SSIENR/SER/BAUDR/DR(FIFO) を PAC 直叩き**（mode0・full-duplex・8bit、SSI 自前 SS は未配線で常時選択）。
- **ESP32 — nina-fw SPI 疎通 (step 2)**（コミット `22dffcc`）: nina-fw に **SPI で `GET_FW_VERSION`(0x37)**
  を投げ **"1.2.2"** 取得。最初は **GPIOHS で bit-bang**（単発は動くが連続コマンドは不安定→step 3 でハードSPI化）。
  配線は MaixPy 既定ドライバ準拠 **EN=IO8 / CS=IO25 / READY=IO9 / SCLK=IO27 / MOSI=IO28 / MISO=IO26**
  （**CS と READY が回路図ネット名と逆**なのが最大の罠）。他のハマり: GPIOHS 関数デフォルトはパッド入力バッファ
  (ie_en)を立てない→READY/MISO で明示有効化。ハンドシェイク: READY low=ready、CS low で high、コマンドと応答は別フレーム。
- **ESP32 リンク確立 — step 1（UART でファーム判定）**（コミット `ebe71cc`）: **IO8=ESP32_EN** をパルスして
  リセット → UART1(IO6/IO7,115200) で **ブートバナーを捕捉**（`ets Jun 8 2016 … SPI_FAST_FLASH_BOOT … entry
  0x4008068c`）→ ESP32 が生きてファーム在中を確認。`AT\r\n`×3 は**無応答** ＝ esp-at ではなく **nina-fw(SPI)**。
- **RTC（リアルタイムクロック）+ mtime クロスチェック**（コミット `a82fbd0` + [src/rtc.rs](src/rtc.rs)）:
  HAL に `rtc` は無いので PAC 直叩き（kendryte SDK の手順を移植）。壁時計を 2026-06-16 12:00:00 にセットして
  1Hz で刻ませ、**独立した CLINT `mtime` と突き合わせて 1Hz を裏取り**。RTC は 26MHz クリスタルを
  `initial_count=26_000_000` で割って 1秒、`register_ctrl` の write/read_enable とマスクで書込/読出を切替。
  実機で **RTC 1秒 = mtime 約 7,800,000 ティック**（6サンプルのばらつき僅か7ティック、期待 ~7.80M=CPU/50 と一致）→
  クリスタル由来の RTC と PLL 由来の CPU クロックが 0.01% で一致して相互検証 → `PASS`。
- **カメラ — 動画ストリーム（解像度ランタイム切替）**（コミット `0cd01a9` + [tools/stream.py](tools/stream.py)）:
  OV2640 のフレームを連続ダンプしてシリアル動画に。**解像度は再フラッシュ不要でホストから切替** ──
  UARTHS の **RX**（io4）でコマンドバイトを受け、`'1'`=QQVGA 160×120 / `'2'`=QVGA 320×240 / `'3'`=VGA 640×480
  を `get_image` の合間にポーリングして OV2640+DVP を再構成（フレームバッファは VGA 分を確保し小解像度はその先頭を使用）。
  ヘッダ `IMGSTART <w> <h>` が現在サイズを運ぶのでホストは自動追従。**fps はシリアル帯域（1.5Mbaud≈120KB/s）で決まる**:
  QQVGA(38KB)≈**2.4fps** / QVGA(154KB)≈0.8fps / VGA(614KB)≈0.2fps。`tools/stream.py` が数秒録って実 fps を測り
  **ffmpeg で mp4 化**（DISPLAY 無し環境でも可。ライブ視聴は `ffplay` にパイプ）。本物の動画には WiFi 等の高速転送が要る。
- **カメラ — VGA 640×480 RGB565 撮影**（コミット `0bebeba`）: クリーン RGB の最大解像度（QVGA の4倍）。
  ハマり所: **出力サイズ(0x5a/0x5b)だけ上書きすると `frame_finish` がハングする** ── センサ読み出し窓
  （0x17/0x18/0x19/0x1a/0x32）・DSP スケーラ（0xc0/0xc1/0x50-0x5c）・PCLK 分周（0xd3）を**全部まとめて**
  動かす必要がある。ArduCAM の `640x480_JPEG` の窓/スケーラ表を流用しつつ、最終フォーマットだけ
  JPEG(0xda=0x10)→ RGB565(0xda=0x08) に差し替え（smart-friend/codex と協働で導出）。
  **シリアルは UARTHS 1.5Mbaud**（614400B が**約5秒/枚**、115200 比 約10倍）。以前「高ボーレートは化ける」と
  していたのはホスト側の早すぎる timeout が原因で、1.5M では IMGSTART ヘッダもフレームもバイト単位でクリーン。
  分周は `cpu/baud-1`（cpu=390MHz 固定）で 1.5M→div 259＝厳密に 390e6/260。2M/3M も厳密分周だが化ける
  （io5 の信号品質限界、1.5M が上限。kflash の書き込みも同じ UARTHS を 1.5M で使うので実績あり）。
  ホスト側 [tools/grab.py](tools/grab.py) は **numpy でベクトル化**（RGB565展開・複数枚 temporal median・
  per-row destripe・PNG 化）── 純Python では 640×480 で約60秒かかっていた処理が**0.1秒未満**。
  `uv run python tools/grab.py --frames 5 --out captures/clean.png`（5枚 median の実写真が合計 ~28 秒）。
- **カメラ — UXGA 1600×1200 JPEG 撮影（最大解像度）**（コミット `f5dbb55`）: OV2640 を JPEG/UXGA に設定
  （ArduCAM レジスタ表）、DVP で JPEG バイト列を取り込み、**デバイス上で SOI/EOI(FF D8…FF D9)を探して
  JPEG 部分だけダンプ**（圧縮済み ~150KB）。**正しい UXGA JPEG**（FFD8FFE0…FFD9、上部に実シーン）。
  ただし **DVP データ経路に ~1誤り/15-30KB のバイト誤り**（RGB ではごま塩で不可視・JPEG はリスタート
  マーカー無しのため最初の1誤りで以降全壊）で長い UXGA は化ける ── **この基板の信号品質限界**。クリーン UXGA は
  この DVP 経路では非現実的（smart-friend/codex とも一致。外部 FIFO 付きカメラが要る）。試した対策: Y モード
  （2 PCLK ごとに 1 バイトのサブサンプリングで不可）、FPIOA パッド調整（Schmitt 等／データ線は SPI0 共有で
  個別調整不可）、VGA JPEG（DVP フレーム同期せずハング）。
- **クロック/PLL 読み出し**（コミット `6c3723f`）: sysctl の PLL0/1/2・分周器レジスタをデコードして
  実周波数を算出（`PLLn = 26MHz/(clkr+1)*(clkf+1)/(clkod+1)`、`aclk=PLL0/2^(div+1)`）。実機で
  **PLL0=780MHz / CPU(aclk)=390MHz / APB=195MHz**。独立に UART 校正した CLINT mtime（=aclk/50）と
  クロスチェック → `7.80 vs 7.79 MHz, MATCH`。2 つの独立手法で CPU 周波数が裏取りできた。
- **マシンタイマ割り込み (ISR)**（コミット `14173c0`）: 初の割り込み駆動。CPU は `wfi` で寝て、CLINT
  マシンタイマ ISR が `mtimecmp` 再アーム・tick カウント・IO6(LED) トグル。riscv-rt は mtvec を張るだけ
  なので `mie.MTIE`/`mstatus.MIE` は生 CSR で自前有効化。出力 1 行＝割り込み 1 回、ホスト側の行間が
  **きっかり 0.500s（2 Hz）** で周期を実証。
- **FFT HW アクセラレータ（DMA 駆動）**（コミット `1e8b233`）: 実信号トーン
  `x[n]=10000·cos(2π·8n/64)` を 64 点 FFT → **bin 8（とミラー bin 56）にピーク**、振幅 5000・隣接 0 で `PASS`。
  **K210 FFT は MMIO データ経路を持たない**（FIFO への CPU 書き込みは握り潰される＝実機確認済み）ので、
  送信(RAM→入力FIFO)・受信(出力FIFO→RAM) の DMA 2 チャネルを同時に回して駆動。
- **DMAC ドライバ + メモリ間転送**（[src/dmac.rs](src/dmac.rs), コミット `b713365`）: DesignWare
  AXI DMA に HAL は無い（k210-hal の `dmac` は 32 行スタブ）ので PAC 直叩き。register sequence は唯一の
  完動 Rust 実装 [laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff) から移植し、現行の
  k210-hal/riscv-rt 0.11 に適合。K210 の癖: SRAM は 0x8000_0000 がキャッシュ有り／0x4000_0000 が無し
  別名なので、DMA バッファは**無し別名経由**で扱いコヒーレンシを確保。
- **AES-128 ECB HW アクセラレータ**（コミット `2b4f948`）: PAC 直叩き。FIPS-197 のテストベクタを
  暗号化 → 既知の `69c4e0d8...c55a` と照合し `PASS`。K210 の癖: `endian` レジスタを**鍵書き込みより
  先に**立てる（順序依存）、鍵は語順逆の LE、出力は LE。
- **SHA256 HW アクセラレータ**（コミット `301661d`）: PAC 直叩き。`SHA256("abc")` をハードで
  計算 → 既知値 `ba7816bf...` と照合。K210 の癖: 結果は語順逆＋バイトスワップ、`en` が done で落ちない。
- **CLINT `mtime` タイマ**（コミット `69cfde6`）: nop ループをやめて `mtime` で正確な 1Hz。
  周波数を UART ボーレートで自己校正 → `mtime_hz=7799258` ≈ CPU/50 ＝ ブート時 CPU ~390MHz。

> オンボード RGB LED(IO13) を光らせようとして大迷走した記録は
> [docs/finding-the-led.md](docs/finding-the-led.md)（結論: GPIO 制御は動くが、IO13 の
> RGB は 4.7K で極暗 or 個体死で視認できず。見える LED は IO6）。

ねらいは、CLI 中心の低レイヤ K210 ツールチェーンをきれいに組むことと、その過程で踏んだ
[大量のツールチェーン罠](docs/)を書き残すこと。

## クイックスタート

```sh
# 一度だけ: Arch でシリアルポートにアクセスする権限（実行後 再ログイン）
sudo gpasswd -a "$USER" uucp

# ビルド（ボード不要）
cargo build

# 接続済みボードへ書き込み（objcopy + kflash, flash.sh 参照）
cargo run

# 出力を見る
picocom -b 115200 /dev/ttyUSB0          # 終了は Ctrl-A Ctrl-X
```

シリアルに出るはずの内容:

```
hello
on
off
on
...
```

同時に IO13 の RGB 赤 LED も駆動しているが、**視認できておらず未確認**
（[docs/finding-the-led.md](docs/finding-the-led.md)）。

## 構成

| パス | 内容 |
|------|------|
| [src/main.rs](src/main.rs) | 本体: UARTHS の hello + GPIOHS の LED 点滅 |
| [memory.x](memory.x) | K210 SRAM 配置 **+ `.eh_frame` の破棄**（これが効いている！） |
| [.cargo/config.toml](.cargo/config.toml) | target・リンカ引数・`cargo run` のフラッシャ |
| [flash.sh](flash.sh) | `cargo run` のフック: ELF → `.bin` → `uv run kflash -B maixduino` |
| [rust-toolchain.toml](rust-toolchain.toml) | toolchain と target の固定 |
| [pyproject.toml](pyproject.toml) / [uv.lock](uv.lock) | `kflash` を `uv` で repo 内固定 |
| [docs/](docs/) | ツールチェーンのメモと罠ログ |

## ツールチェーン要約

- target は `riscv64imac-unknown-none-elf`（soft-float。K210 は FPU を*持っている*が、
  クレート群が soft-float なので。詳細は docs）
- ピンmux と UART は **git 版の `k210-hal`**（crates.io の `0.2.0` は古いスタブ）。
  LED は GPIO HAL が未完成なので **PAC レジスタ直叩き**で駆動
- `riscv-rt = "0.11"`（0.8 の `link.x` は今の rust-lld を壊す）
- 書き込みは `kflash`。`uv` で固定し `uv run kflash` で実行

おもしろいところ — float ABI の不一致、`(*(.trap))` のリンカスクリプト破綻、
K210 の SRAM が `0x8000_0000` にあるせいだけで踏む `.eh_frame` の再配置オーバーフロー —
は **[docs/](docs/)** に詳述。

## ステータス

**実機で確認済み**: ビルド / 書き込み（`uv run kflash -B maixduino`）/ シリアル出力（UARTHS）/
**IO6 の赤 LED 点滅**（Rust でも公式 MaixPy でも光る = GPIO 制御は両スタックで動作）。

**未決着**: ドキュメント上の RGB 赤 LED（IO13, 4.7K）の点灯。回路図上は正しく駆動しているが
視認できない —— IO6(510R, 明るい) の約10倍暗い ~0.3mA が見えないのか、個体の LED 死/断線か。
テスタ実測待ち。経緯は [docs/finding-the-led.md](docs/finding-the-led.md)。

カメラ(DVP)と WiFi(オンボード ESP32)は到達可能。HAL クレートに DVP 抽象は無いが、
`k210-pac` に DVP レジスタブロック（SCCB 含む）が有り、Rust の完動例
（[laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff) の `dvp-ov`, OV2640）も
存在する → 移植で行ける。詳細は [docs/hardware-maixduino.md](docs/hardware-maixduino.md)。

参照した回路図・データシートは [hardware/](hardware/) に確保（出典 dl.sipeed.com）。

> コード中のコメントは英語のままにしてある（必要なら日本語化する）。
