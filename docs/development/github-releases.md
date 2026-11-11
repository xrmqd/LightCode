# Сборки и релизы LightCode на GitHub

Workflow `.github/workflows/ci.yaml` собирает macOS только для Apple Silicon
на стандартном GitHub-hosted runner `macos-26`. Сборка для Intel временно отключена.
Linux используется только для загрузки готовых файлов в релиз, без сборки редактора.
Workflow для Codeberg/Forgejo удалены; шаблоны issues сохранены.

## Выпуск версии

1. Укажи версию в `[workspace.package]` в `Cargo.toml` и обнови `Cargo.lock`,
   если версия или зависимости менялись. Версия приложения берётся из Cargo,
   а не из имени тега.
2. Закоммить изменения, создай тег, например `v3.5.1-fork.1`, и отправь его
   в свой GitHub-репозиторий (`origin`). Теги не следует переносить на другой коммит.
3. GitHub Actions соберёт версию для Apple Silicon. После успешной сборки создаст
   GitHub Release и приложит `LightCode.dmg`, remote-server, архив соответствующих
   исходников и контрольные суммы. Теги с дефисом создают prerelease.

Для проверки открой Actions → LightCode macOS builds and releases → Run workflow.
Без поля `tag` сборка выбранной ветки сохранит файлы только в Artifacts
(на 14 дней). Если выбрана ссылка на тег или заполнено поле `tag`, файлы
также публикуются в релиз. Поле принимает существующий тег, не ветку.
Повторный запуск заменяет одноимённые файлы существующего релиза.

Используется встроенный `GITHUB_TOKEN`; отдельный PAT не нужен. Для публикации
workflow должен иметь возможность получить `contents: write` в настройках
репозитория или организации. Не отправляй сразу все унаследованные теги
(`git push --tags`): новые push-события для `v*` запустят сборки этих версий.

## Подпись macOS

Сборки имеют ad-hoc подпись (`codesign --sign -`), без Developer ID и notarization.
Сертификаты и аккаунт Apple не требуются. macOS может блокировать первый запуск;
разрешение выдаётся вручную в настройках «Конфиденциальность и безопасность».
Профиль разработчика оригинального Gram в эти сборки не копируется.
Для Developer ID signing и notarization понадобится отдельная настройка своих
сертификатов и секретов.

Полный Xcode и Metal toolchain устанавливаются на раннере; Rust берётся из
`rust-toolchain.toml`, зависимости Cargo фиксируются через `--locked`.

Установщик всегда называется `LightCode.dmg`, приложение — `LightCode.app`.
В Artifacts установщик лежит в архиве `macos-aarch64`.
В GitHub Releases публикуется непосредственно `LightCode.dmg` для Apple Silicon.
В релиз также входят `lightcode-remote-server-macos-aarch64.gz`,
`LightCode-source.tar.gz` и `SHA256SUMS.txt`.
Адрес собственного репозитория и точный тег встраиваются workflow через
`LIGHTCODE_REPOSITORY_URL` и `LIGHTCODE_RELEASE_TAG`; SSH скачивает сервер
только из релиза этого форка, а не из Gram. Готовые remote-server сейчас
собираются только для macOS на Apple Silicon; для Intel и Linux нужна отдельная сборка.
