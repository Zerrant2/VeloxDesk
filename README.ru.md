<div align="center">

# VeloxDesk

**Быстрый, лёгкий, open-source удалённый рабочий стол на чистом Rust.**  
Администрирование уровня RustDesk + низколатентный игровой режим уровня Parsec — без платных Pro-ограничений и без тяжёлых рантаймов Electron/Flutter/C++.

[![GitHub Release](https://img.shields.io/github/v/release/Zerrant2/VeloxDesk?style=flat-square)](https://github.com/Zerrant2/VeloxDesk/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](LICENSE-MIT)
[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange?style=flat-square)](Cargo.toml)

**[English](README.md)** | **Русский**

</div>

---

## Возможности

### Единое приложение Хост + Клиент (`veloxdesk`)
- **Всё в одном окне**: сразу работает и на приём подключений («Этот компьютер»), и как клиент для подключения к удалённым ПК на лёгком стеке `wgpu` + `egui`.
- **Системный трей и служба ОС**: сворачивается в системный трей Windows, поддерживает портативный режим `--portable`, локальный IPC и установку как системной службы Windows SCM / Linux systemd (`--service-install`).
- **Русский и английский интерфейс**: полная локализация UI, HUD, настроек, всплывающих подсказок и меню трея с сохранением выбранного языка.
- **Автообновление с GitHub Releases**: фоновая проверка новых версий по HTTPS (`rustls`), проверка PE-заголовка и SHA-256 дайджеста из GitHub API, атомарная подмена `veloxdesk.exe` с откатом при ошибке и автоматический перезапуск.

### Игровой режим уровня Parsec
- **Независимые профили `Desktop` и `Gaming`**: все параметры трансляции (качество/битрейт, лимит FPS до 240, режим презентации, масштабирование, HUD, аппаратный декодер, звук, микрофон, буфер обмена, HDR, YUV 4:4:4, относительная мышь) настраиваются и сохраняются отдельно для каждого режима.
- **GPU Zero-Copy пайплайн**: захват экрана через DXGI Desktop Duplication и DX12 GPU Zero-Copy (`~3.03 ms` p50 на 1080p120 и `~3.23 ms` p50 на 1080p240 в локальных бенчмарках).
- **Аппаратное видео и HDR**: NVENC / D3D11VA / Media Foundation (с программным fallback на `openh264`), **10-bit HDR** (`P010` / HEVC Main10 / AV1 10-bit) и **YUV 4:4:4** (чёткий текст без замыливания).
- **Звук и геймпады**: чистый Rust Opus (`rusty-opus`) + WASAPI loopback/микрофон и до **4 геймпадов** (XInput → ViGEmBus / uinput) с поддержкой вибрации (rumble).

### Администрирование и сеть уровня RustDesk
- **Гибкие способы подключения**:
  - Прямое подключение по **LAN / IP** (`UDP 21118`).
  - **Нативная интеграция с Tailscale**: автообнаружение `tailscale.exe`, копирование своего IP в 1 клик и список онлайн-устройств для быстрого подключения.
  - **Подключение по 9-значному ID**: собственный self-hosted сервер `veloxdesk-server` с UDP NAT hole punching (`UDP 21116`) и резервным zero-knowledge ретранслятором (`UDP 21117`).
- **Надёжный транспорт**: QUIC (`quinn`) — ненадёжные датаграммы + Reed-Solomon FEC + адаптивный битрейт (ABR) для видео/аудио и надёжные потоки для управления, файлов, туннелей и терминала.
- **Инструменты**:
  - Двусторонняя синхронизация буфера обмена (текст, HTML, PNG, файлы с `zstd`-сжатием и подавлением эха).
  - Файловый менеджер с докачкой, `zstd`-сжатием и проверкой SHA-256.
  - Встроенный удалённый терминал (**ConPTY** на Windows / PTY на Unix).
  - Проброс TCP-портов (`--tunnel local:host:remote`).
  - Переключение мониторов, виртуальный дисплей и экран приватности (`PrivacyGuard`).

### Безопасность и контроль доступа
- **Сквозное шифрование (E2EE)**: QUIC + TLS 1.3 с проверкой постоянных Ed25519-ключей (`identity.key`). Сервер-посредник не имеет доступа к содержимому сессии.
- **Пароли без передачи в открытом виде**: аутентификация challenge-response (`Argon2id` + `HMAC-SHA256`), на диске хранится только Argon2id-хэш.
- **Три режима пароля на хосте**: **Только одноразовый (OTP)** (меняется после каждого входа), **Только постоянный** (для доступа без подтверждения) или **Оба одновременно**.
- **Окно подтверждения и доверенные устройства**: диалог `AcceptPrompt` с гранулярными правами и галочкой *«Запомнить это устройство»* — доверенные ПК сохраняются на диск и могут быть мгновенно отозваны в «Настройках».

---

## Быстрый старт

### 1. Скачать готовые бинарники для Windows
Скачайте `veloxdesk.exe` и `veloxdesk-server.exe` со страницы [**GitHub Releases**](https://github.com/Zerrant2/VeloxDesk/releases/latest).

### 2. Прямое подключение (локальная сеть или Tailscale)
1. Запустите `veloxdesk.exe` на обоих компьютерах.
2. На хосте посмотрите **LAN IP** или **Tailscale IP** (`100.x.y.z`) и пароль (одноразовый OTP или постоянный пароль из **Настройки → Доступ к этому ПК и безопасность**).
3. На клиенте введите IP (или нажмите на устройство в списке Tailscale) и кликните **Подключиться**.

### 3. Свой сервер для подключения по 9-значному ID (`veloxdesk-server`)

```sh
veloxdesk-server \
  --rendezvous-bind 0.0.0.0:21116 \
  --relay-bind 0.0.0.0:21117 \
  --public-relay-addr <ВАШ_ПУБЛИЧНЫЙ_IP>:21117
```

Откройте UDP-порты **`21116`** (Rendezvous / пробитие NAT) и **`21117`** (Zero-Knowledge Relay) в файрволе сервера, затем укажите `<ВАШ_ПУБЛИЧНЫЙ_IP>:21116` в поле **Rendezvous-сервер** на обоих ПК.

### 4. Полезные команды CLI

```sh
# Обычный запуск GUI (Хост + Клиент)
veloxdesk

# Портативный режим (настройки и identity.key рядом с veloxdesk.exe)
veloxdesk --portable

# Прямое подключение в игровом режиме с HUD и TCP-туннелем
veloxdesk --connect 100.64.0.10:21118 --gaming --hud --tunnel 8080:127.0.0.1:80

# Запуск автономного консольного хоста
veloxdesk --host --config veloxdesk.toml

# Установка / статус / удаление системной службы (Windows SCM / Linux systemd)
veloxdesk --service-install
veloxdesk --service-status
veloxdesk --service-uninstall
```

---

## Где хранятся настройки и ключи

| ОС / Режим | Путь | Файлы |
|---|---|---|
| **Windows** | `%APPDATA%\veloxdesk\` | `settings.json` (профили, язык, недавние, хэш постоянного пароля, доверенные устройства), `identity.key` (Ed25519-ключ и 9-значный ID) |
| **Linux** | `$XDG_CONFIG_HOME/veloxdesk/` или `~/.config/veloxdesk/` | `settings.json`, `identity.key` |
| **macOS** | `~/Library/Application Support/veloxdesk/` | `settings.json`, `identity.key` |
| **Портативный (`--portable`)** | Папка рядом с `veloxdesk.exe` | `veloxdesk-settings.json`, `identity.key`, опционально `veloxdesk.toml` |

---

## Сборка из исходников

Требуется **Rust 1.85+** (Rust 2024 edition).

```sh
git clone https://github.com/Zerrant2/VeloxDesk.git
cd VeloxDesk

# Сборка релизных бинарников
cargo build --release -p veloxdesk -p veloxdesk-server

# Проверка clippy и всех тестов workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Подробная архитектура, ADR и спецификация протокола описаны в [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

---

## Горячие клавиши (в сеансе)

| Комбинация | Действие |
|---|---|
| `Ctrl+Shift+F` | Переключить полноэкранный режим |
| `Ctrl+Alt+G` | Переключить захват мыши в игровом режиме |
| `Ctrl+Shift+H` | Показать / скрыть оверлей статистики (HUD) |

---

## Лицензия

Двойная лицензия на выбор: [MIT](LICENSE-MIT) или [Apache-2.0](LICENSE-APACHE).
