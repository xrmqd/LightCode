# LightCode

LightCode — независимый форк редактора [Gram](https://codeberg.org/GramEditor/gram),
который, в свою очередь, основан на [Zed](https://github.com/zed-industries/zed).
Проект развивается отдельно от Gram; при доработке LightCode допускается помощь ИИ.

Редактор сохраняет основные возможности исходного проекта: LSP, встроенный Git,
отладчик DAP, REPL/Jupyter, Vim/Helix-режимы и расширения.

## Сборки macOS

GitHub Actions собирает **LightCode.app** для Apple Silicon и Intel.
Отправка тега `v…` создаёт GitHub Release с DMG, соответствующими исходниками,
macOS remote-server и контрольными суммами. Возможен ручной запуск сборки.

Сборки имеют ad-hoc подпись без Developer ID и notarization. macOS может
потребовать разрешить первый запуск в настройках безопасности.

- [Выпуск релизов на GitHub](docs/development/github-releases.md)
- [Сборка macOS из исходников](docs/development/macos.md)
- [Документация редактора](docs/index.md)

## Идентичность форка

- Имя приложения: `LightCode` (`LightCode Dev` для dev-канала).
- Идентификатор macOS: `dev.lightcode.editor` (dev: `dev.lightcode.editor-dev`).
- Команда после установки CLI через меню: `lightcode`.
- Ссылки внутри редактора: `lightcode://…`.
- Настройки: `~/.config/lightcode`; данные macOS:
  `~/Library/Application Support/LightCode`; журналы: `~/Library/Logs/LightCode`.

Внутренние имена Rust-пакетов (`gram`, `cli`), исполняемый файл внутри .app,
названия API и формат проектной папки `.gram` сохранены для совместимости.
Некоторые унаследованные инструкции и материалы описывают именно Gram.
Его манифест и правила участия не являются правилами этого независимого форка.

## Происхождение и лицензия

Изменённая версия от **11 ноября 2026 года**: переименование в LightCode,
отдельная идентичность приложения и сборки macOS с публикацией на GitHub.

LightCode распространяется под GNU GPLv3 с сохранением исходных уведомлений
об авторстве и условий компонентов. См. [LICENSE](LICENSE) и
[лицензии зависимостей](assets/licenses.md).

Авторские права исходных проектов Gram и Zed сохраняются. LightCode не является
официальной версией Gram и не подразумевает одобрение его разработчиков.
Сведения об исходных авторах: [Gram credits](https://gram-editor.com/credits).
