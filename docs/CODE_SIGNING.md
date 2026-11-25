# Бесплатная подпись через SignPath Foundation

## Текущее состояние

По решению владельца от 25 ноября 2026 подпись отложена на неопределённый срок и может быть полностью исключена. Это не обязательный следующий этап. Приведённые ниже инструкции сохранены только на случай нового решения владельца; заявку сейчас не подаём и интеграцию не включаем.

Интеграция подготовлена, но **SignPath Foundation ещё не одобрила MineHost**. Сертификат не получен, подписанных релизов пока нет. Удаление предупреждения SmartScreen не гарантируется даже после подписи. Самоподписанные сертификаты, отключение Defender и установка собственных доверенных корней не используются.

## Что делает владелец проекта

1. Откройте [заявку SignPath Foundation](https://signpath.org/apply). Текст для неё находится в [.signpath/application.md](../.signpath/application.md). Укажите свой контакт непосредственно в форме, не в GitHub-файлах и не в чате.
2. Ознакомьтесь с [условиями](https://signpath.org/terms), подтвердите контроль репозитория, MFA и роли. MIT, публичные исходники и опубликованные установщики уже есть, но принятие нового проекта зависит от проверки Foundation; оно не гарантировано.
3. После одобрения подключите SignPath GitHub App к **Frish07/MineHost**, добавьте доверенную систему сборки GitHub.com и production signing policy с ручным подтверждением и ограничением на `v*` tags. Провайдер может потребовать дополнительные правила review/защиты веток: их нужно согласовать с Foundation.
4. Создайте две artifact configurations: `minehost-app` по [application.xml](../.signpath/application.xml) и `minehost-installer` по [installer.xml](../.signpath/installer.xml). Это шаблоны по официальному XML-формату; окончательную схему и ограничения должен подтвердить SignPath. Расскажите им, что отдельной подписи генерируемого NSIS uninstaller пока нет.
5. В Settings → Secrets and variables → Actions добавьте секрет `SIGNPATH_API_TOKEN` с правом submitter. **Не присылайте токен в чат и не коммитьте его.**
6. Там же добавьте repository variables:

| Variable | Значение |
| --- | --- |
| `SIGNPATH_ORGANIZATION_ID` | Выданный SignPath organization ID |
| `SIGNPATH_PROJECT_SLUG` | Slug проекта MineHost в SignPath |
| `SIGNPATH_SIGNING_POLICY_SLUG` | Slug production policy с ручным подтверждением |
| `SIGNPATH_APP_CONFIGURATION_SLUG` | `minehost-app`, если это имя выбрано при создании |
| `SIGNPATH_INSTALLER_CONFIGURATION_SLUG` | `minehost-installer`, если это имя выбрано при создании |
| `WINDOWS_SIGNING_ENABLED` | `true` только после одобрения и настройки всех предыдущих значений |

7. Выпустите следующую версию с новым тегом после успешного Build. Старые подписанные/неподписанные файлы релизов не заменяются. Дважды подтвердите запросы в SignPath: сначала приложение, затем установщик. Runner ждёт каждый запрос до 30 минут; без одобрения выпуск остановится.

## Порядок сборки

Проверки кода → production EXE и черновой NSIS через `tauri build --bundles nsis` для инициализации bundle metadata → upload GitHub artifact → подпись подготовленного EXE в SignPath → проверка Authenticode и timestamp → повторная упаковка NSIS через `tauri bundle --no-binary-patching` → upload installer artifact → подпись установщика → повторная проверка подписей → SHA-256 → Release. Промежуточный черновой установщик не публикуется.

`--no-binary-patching` сохраняет подписанный EXE без изменения байтов. Это подходит текущему единственному формату NSIS; updater с несколькими типами bundle сейчас не используется. После упаковки workflow сравнивает хеш EXE до и после. При включённой подписи unsigned fallback запрещён. При выключенной настройке обычные сборки продолжаются без обращения к SignPath и явно остаются unsigned.

После одобрения Foundation обновите статус в CODE_SIGNING_POLICY.md и README, сохранив пометку старых релизов как unsigned. Настройте доверенную временную метку для Authenticode в SignPath: без timestamp проверка не примет результат.

Приватный ключ остаётся у сервиса. На компьютере пользователя ничего не устанавливается; все средства сборки и проверки подписей работают только на `windows-latest`.

Источники: [SignPath GitHub integration](https://docs.signpath.io/trusted-build-systems/github), [artifact configuration](https://docs.signpath.io/artifact-configuration/syntax), [Tauri build/bundle](https://v2.tauri.app/distribute/).
