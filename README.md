## Backend для мобилки "Friends"

### Запуск
1) установка Docker
2) curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh # установка компилятора rust
3) open ~/.zshrc
4) добавить: export PATH="$HOME/.cargo/bin:$PATH"
5) закрытие терминала
6) which rustc (если пусто сделай source ~/.zshrc и переоткрыть терминал)
7) cd Server
8) make all (запуск db -> server)
