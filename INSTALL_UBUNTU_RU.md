# Установка «Контроль ИД» v0.5 на Ubuntu Server 24.04 LTS

> Размещение: PostgreSQL и Rust/Axum на **одном Ubuntu-сервере**, Synology доступна **удалённо по HTTPS 5001**. Никаких SMB/CIFS-mount, WebDAV или внешнего IP для самого приложения не требуется. Это пилотный проект, не допущенный к промышленной эксплуатации без тестов и ролевого разграничения.

## 0. Схема сети и папок

```text
Ubuntu 24.04: PostgreSQL localhost:5432 + id-control-axum localhost:8090
      | HTTPS исходящее соединение на NAS:5001
      v
Synology DSM: https://nas.example.local:5001
Каталог File Station: /resdata/Исполнительная документация
  ЖК Северный/
    Земляные работы/
      ИД/АОСР №15.docx
      ИД/Реестр.xlsx
      ПД-РД/Рабочие чертежи.pdf
```

**Важно:** `/resdata` — название **общей папки DSM**, не `/volume1/resdata`. Указать точную папку в `SYNOLOGY_ROOT`, включая регистр и пробелы. Сканирование идёт **только внутри этой папки** (рекурсивно). Проекты пока создаются во внутренней PostgreSQL, **не в Bitrix24**.

## 1. Подготовка DSM

1. Создайте пользователя DSM, например `id_reader`, не администратора. Дайте право **только чтения** соответствующей общей папки, включив доступ по File Station, а не только через SMB.
2. Убедитесь, что этот пользователь через интерфейс File Station видит нужный каталог и может скачивать файлы. Выключенный download/list в расширенных настройках общего ресурса не даст API работать.
3. С сервера Ubuntu должен открываться `https://nas.example.local:5001`. Если доступ организован через интернет, ограничьте вход на 5001 IP сервера или VPN; не открывайте NAS произвольно для всех.
4. Предпочитайте DNS-имя NAS, совпадающее с именем в SSL-сертификате. При корпоративном CA установите его PEM на Ubuntu и укажите `SYNOLOGY_CA_CERT`. По умолчанию проверка TLS **включена**.
5. Если DSM требует интерактивную двухфакторную авторизацию, согласуйте отдельную подходящую для сервисного доступа учётную запись и разрешения: данная версия не поддерживает ввод одноразового кода.

Проверить сеть с Ubuntu:

```bash
curl -v 'https://nas.example.local:5001/webapi/entry.cgi?api=SYNO.API.Info&version=1&method=query&query=SYNO.FileStation.List'
```

При самоподписанном сертификате `curl` сообщит об ошибке TLS. Рекомендуется подключить доверенный сертификат/корпоративный CA вместо отключения проверки.

## 2. Ubuntu: системные пакеты

```bash
sudo apt update
sudo apt install -y postgresql postgresql-client \
  build-essential pkg-config libssl-dev ca-certificates curl unzip \
  poppler-utils tesseract-ocr tesseract-ocr-rus tesseract-ocr-eng \
  libreoffice-writer fonts-dejavu-core nginx
sudo systemctl enable --now postgresql
sudo systemctl status postgresql --no-pager
```

Rust toolchain можно установить через rustup под своей учётной записью разработчика:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version
cargo --version
```

На закрытом сервере переносите заранее собранный совместимый бинарный файл вместо запуска rustup.

## 3. Настройка локальной PostgreSQL

PostgreSQL должен слушать **только loopback**. На Ubuntu `127.0.0.1:5432` обычно разрешён по умолчанию. Создать учётную запись и базу:

```bash
sudo -u postgres psql
```

Внутри psql:

```sql
CREATE ROLE idcontrol WITH LOGIN;
\password idcontrol
CREATE DATABASE idcontrol OWNER idcontrol ENCODING 'UTF8';
\q
```

Команда `\password` вводит пароль интерактивно, не записывая его в историю shell. Программа выполняет миграции при первом запуске. При запуске под существующей БД пользователь `idcontrol` должен быть владельцем схемы и иметь право `CREATE` в ней.

В конфигурации PostgreSQL проверьте значение `listen_addresses`: `localhost`; в `pg_hba.conf` убедитесь, что подключения с `127.0.0.1/32` используют `scram-sha-256`. Путь к этим файлам зависит от версии PostgreSQL:

```bash
sudo -u postgres psql -Atqc 'SHOW config_file;'
sudo -u postgres psql -Atqc 'SHOW hba_file;'
```

После изменения параметров, если это потребовалось:

```bash
sudo systemctl restart postgresql
```

Проверить подключение (пароль будет запрошен):

```bash
psql -h 127.0.0.1 -p 5432 -U idcontrol -d idcontrol -c 'SELECT current_database(), current_user;'
```

**Пароль внутри `DATABASE_URL`:** символы `@`, `:`, `/`, `%`, `#` и другие специальные символы требуют URL-кодирования. Проще использовать сгенерированный буквенно-цифровой пароль или процентное кодирование.

## 4. Развёртывание Rust-приложения

Распакуйте архив, перейдите в директорию `id-control-axum`:

```bash
unzip Контроль_ИД_Rust_Axum_HTTPS_PostgreSQL_v0.5.zip
cd id-control-axum
cp .env.example .env
nano .env
```

Пример ключевых настроек (не копируйте демонстрационные пароли):

```dotenv
DATABASE_URL=postgresql://idcontrol:DB_PASSWORD@127.0.0.1:5432/idcontrol
LISTEN=127.0.0.1:8090
APP_USER=admin
APP_PASSWORD=LONG_APP_PASSWORD
SYNOLOGY_URL=https://nas.example.local:5001
SYNOLOGY_USERNAME=id_reader
SYNOLOGY_PASSWORD=NAS_PASSWORD
SYNOLOGY_ROOT=/resdata/Исполнительная документация
SYNOLOGY_VERIFY_TLS=true
DATA_ROOT=/var/lib/id-control
POLL_INTERVAL=45
```

Для своего CA раскомментируйте `SYNOLOGY_CA_CERT=/etc/id-control-axum/synology-ca.pem`. Только как временную диагностическую меру в доверенной сети можно поставить `SYNOLOGY_VERIFY_TLS=false` — это ослабляет защиту подключения.

```bash
chmod 600 .env
cargo check
cargo test
cargo build --release
```

Проверка NAS без обращения к БД и без скачивания файлов:

```bash
./target/release/id-control-axum --check-synology
```

Ожидаемый вывод:

```text
Synology HTTPS connection OK. Files in configured folder: N
```

При неверном `SYNOLOGY_ROOT`, TLS, правах, пароле или сетевом доступе будет ошибка. Проверьте путь именно как в File Station.

## 5. Установка systemd

Предположим, исходники и собранный бинарник находятся в домашней директории текущего пользователя:

```bash
sudo useradd --system --home /var/lib/id-control --shell /usr/sbin/nologin idcontrol || true
sudo install -d -o idcontrol -g idcontrol -m 700 /var/lib/id-control
sudo install -d -o root -g root -m 755 /opt/id-control-axum
sudo install -o root -g root -m 755 target/release/id-control-axum /opt/id-control-axum/id-control-axum
sudo install -o root -g root -m 600 .env /etc/id-control-axum.env
sudo install -o root -g root -m 644 deploy/id-control-axum.service /etc/systemd/system/id-control-axum.service
sudo systemctl daemon-reload
sudo systemctl enable --now id-control-axum
sudo systemctl status id-control-axum --no-pager
sudo journalctl -u id-control-axum -f
```

Если используется `SYNOLOGY_CA_CERT`, сделайте сертификат доступным для чтения пользователю `idcontrol` (обычный публичный сертификат CA не является секретом). Пути `DATA_ROOT` и `ReadWritePaths` в systemd должны совпадать. **Секреты не должны находиться в общедоступных файлах или репозитории Git.**

Проверка HTTP из Ubuntu (авторизация Basic Auth):

```bash
curl -u admin -i http://127.0.0.1:8090/health
```

Команда запросит пароль. `/health` покажет `synology_connected: true` после первого успешного полного опроса; до него возможен `false`.

## 6. Доступ сотрудников

Axum слушает `127.0.0.1:8090`, поэтому не публикуется напрямую в сеть. Внутренний HTTPS организуйте через Nginx (шаблон `deploy/nginx.conf`) и корпоративное DNS-имя, например `id-control.intra.example`. Установите настоящий сертификат на Nginx и откройте 443 только из корпоративной сети/VPN. PostgreSQL 5432 не открывайте извне.

При удалённом доступе предпочтителен корпоративный VPN. Для интеграции в Bitrix24 потребуется отдельный проект интеграции, а не просто изменение порта.

## 7. Схема рабочего процесса

1. Сервис раз в `POLL_INTERVAL` секунд авторизуется в DSM и постранично перечисляет содержимое `SYNOLOGY_ROOT`.
2. По размеру и времени изменения определяет новые/изменённые файлы, после задержки `STABLE_SECONDS` скачивает их в локальную временную папку. **На NAS ничего не записывается.**
3. Программа сохраняет неизменяемую копию в `DATA_ROOT/originals`, вычисляет SHA-256, классифицирует документ и заносит версию в PostgreSQL.
4. Папка `ЖК Северный/Земляные работы` становится внутренним проектом и комплектом; проверка идёт по утверждённому чек-листу и утверждённым нормативным правилам.
5. Инженер просматривает результаты на `/packages/{id}`, подтверждает замечания и выгружает отчёт. Проекты в Bitrix24 текущая версия **не создаёт**.
6. Если NAS недоступен или листинг неполон — операции по пометке исчезнувших документов **не выполняются**. Для пустого каталога массовое исчезновение документов автоматически не подтверждается.

## 8. Проверка ошибок

| Симптом | Где искать |
|---|---|
| `DSM login failed` | учётная запись, пароль, доступ в File Station, 2FA |
| `certificate verify failed` | имя в SAN сертификата, локальный CA, `SYNOLOGY_CA_CERT` |
| `DSM list failed` | `SYNOLOGY_ROOT` и права чтения конкретной папки |
| `Incomplete DSM folder listing` | API, лимиты, тайм-ауты, сеть; данные не считаются удалёнными |
| `connection refused` на 5432 | `systemctl status postgresql`, `listen_addresses`, `pg_hba.conf` |
| Ошибка при `CREATE TABLE` | владелец БД и права пользователя в схеме |
| Приложение не открывается с другого ПК | прокси Nginx, корпоративный DNS, сертификат, firewall 443 |

### Резервное копирование PostgreSQL

```bash
sudo install -d -o postgres -g postgres -m 700 /var/backups/id-control
sudo -u postgres pg_dump -Fc -d idcontrol -f /var/backups/id-control/idcontrol.dump
```

Бэкап БД **не содержит архивы документов** из `/var/lib/id-control/originals`; сохраняйте их отдельно, проверяйте восстановление. Не считайте исходный каталог Synology единственным бэкапом системы.
