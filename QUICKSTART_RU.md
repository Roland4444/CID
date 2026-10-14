# Краткая настройка — Ubuntu + PostgreSQL + Synology HTTPS

1. Установите пакеты: `sudo apt install -y postgresql postgresql-client poppler-utils tesseract-ocr tesseract-ocr-rus tesseract-ocr-eng libreoffice-writer build-essential pkg-config libssl-dev ca-certificates`.
2. Выполните `sudo -u postgres psql`, затем `CREATE ROLE idcontrol WITH LOGIN;`, `\password idcontrol`, `CREATE DATABASE idcontrol OWNER idcontrol;`, `\q`.
3. Установите Rust/cargo, убедитесь, что сервер Ubuntu видит по сети Synology HTTPS `5001`, а пользователь NAS имеет права чтения папки через File Station.
4. `cp .env.example .env` и заполните `DATABASE_URL=postgresql://idcontrol:пароль@127.0.0.1:5432/idcontrol`, `APP_PASSWORD`, `SYNOLOGY_URL=https://...:5001`, `SYNOLOGY_USERNAME`, `SYNOLOGY_PASSWORD`, `SYNOLOGY_ROOT=/имя_шары/папка`.
5. `cargo check && cargo test && cargo build --release`.
6. `./target/release/id-control-axum --check-synology` — проверка DSM без подключения БД; затем `./target/release/id-control-axum` — запуск с PostgreSQL.
7. Для фонового запуска перенесите бинарник в `/opt/id-control-axum/`, установите `deploy/id-control-axum.service` и положите конфиг в `/etc/id-control-axum.env` с правами `600`.

Полная инструкция с точными командами и диагностикой — **`INSTALL_UBUNTU_RU.md`**.

**Примечание:** исходники ещё не прошли сборку в данной среде (нет Rust toolchain). Инженерные проверки ГОСТ требуют утверждённых нормативных правил и не заменяют проверку специалиста.
