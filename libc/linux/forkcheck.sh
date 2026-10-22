#!/bin/bash
# Оболочка Linux на процессах (фаза 71d): подстановка команды, конвейер,
# коды выхода детей, сценарий через `#!`, фоновая задача, `wait` и `kill`.
#
# Едет в пакете `debian-12.fpk` как `/usr/libexec/fossai/forkcheck`; строки —
# `forkcheck: ok <что>` или `forkcheck: FAILED <что>`, итог — `forkcheck: N
# failed`, код выхода — то же N. Ядерная оболочка не знает кавычек, поэтому
# всё, что требует `bash -c '…'`, живёт здесь.

if [ "$1" = "--child" ]; then
    echo "child $2"
    exit 0
fi

fails=0
ok() {
    if [ "$2" = "$3" ]; then
        echo "forkcheck: ok $1"
    else
        echo "forkcheck: FAILED $1 (got '$2', want '$3')"
        fails=$((fails + 1))
    fi
}

ok "command substitution" "$(echo inner)" "inner"
ok "a pipeline of two programs" "$(echo one two three | wc -w)" "3"
ok "ls into head" "$(ls /lib64 | head -n 1)" "ld-linux-x86-64.so.2"
/bin/false
ok "exit status of a child" "$?" "1"
(exit 5)
ok "exit status of a subshell" "$?" "5"
ok "a script runs through #!" "$(/usr/libexec/fossai/forkcheck --child 7)" "child 7"
ok "environment reaches a child" "$(FOSSAI=yes bash -c 'echo $FOSSAI')" "yes"
ok "PPID of a child shell" "$(bash -c 'echo $PPID')" "$$"
sleep 0.2 &
wait $!
ok "a background job and wait" "$?" "0"
sleep 30 &
victim=$!
kill $victim
wait $victim
ok "kill a background job" "$?" "143"

echo "forkcheck: $fails failed"
exit $fails
