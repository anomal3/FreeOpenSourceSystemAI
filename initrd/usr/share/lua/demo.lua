-- Образец: что умеет Lua, запущенная в FreeOS.
--
-- Сценарий не украшение, а проверка. Каждая строка здесь трогает свой слой:
-- виртуальную машину, сборщик мусора, стандартную библиотеку языка и — в конце
-- — саму систему, через файлы и запуск другой программы. Печатает он строки
-- вида `demo: <что> <результат>`: по ним стенд и проверяет, что язык не просто
-- запустился, а считает.
--
-- Запуск:  lua /usr/share/lua/demo.lua [каталог для временных файлов]
-- Каталог по умолчанию — текущий; на установленной системе это /home/<кто вошёл>.

local where = arg and arg[1] or "."

print("demo: " .. _VERSION)

-- Числа. Целые и дробные в Lua 5.4 — разные подтипы, и `//` с `%` обязаны
-- считать по её правилам, а не по правилам C.
local sum = 0
for i = 1, 100 do
  sum = sum + i
end
print(string.format("demo: sum 1..100 = %d, 7//2 = %d, 7%%2 = %d, sqrt(2) = %.5f",
                    sum, 7 // 2, 7 % 2, math.sqrt(2)))

-- Строки: свой сборщик строк, свои шаблоны поиска.
local text = "FreeOpenSourceSystemAI"
local vowels = select(2, text:gsub("[aeiouAEIOU]", ""))
print(string.format("demo: %s has %d letters and %d vowels, upper starts %s",
                    text, #text, vowels, text:sub(1, 7):upper()))

-- Таблицы и сортировка: массив, хеш и `table.sort` с чужим сравнением.
local names = { "sshd", "httpd", "init", "dhcp", "lua" }
table.sort(names)
print("demo: sorted " .. table.concat(names, " "))

local counts = {}
for _, name in ipairs(names) do
  counts[name] = #name
end
print(string.format("demo: httpd is %d letters, lua is %d", counts.httpd, counts.lua))

-- Замыкания: настоящие, с общим состоянием.
local function counter()
  local n = 0
  return function()
    n = n + 1
    return n
  end
end
local tick = counter()
tick(); tick()
print("demo: closure counted " .. tick())

-- Метатаблицы: перегрузка сложения — это уже работа виртуальной машины.
local vector = {}
vector.__index = vector
vector.__add = function(a, b) return setmetatable({ x = a.x + b.x, y = a.y + b.y }, vector) end
vector.__tostring = function(v) return "(" .. v.x .. "," .. v.y .. ")" end
local a = setmetatable({ x = 1, y = 2 }, vector)
local b = setmetatable({ x = 3, y = 4 }, vector)
print("demo: vector " .. tostring(a + b))

-- Сопрограммы: своя передача управления, без единого потока в системе.
local co = coroutine.create(function(first)
  local total = first
  for i = 1, 3 do
    total = total + coroutine.yield(total)
  end
  return total
end)
local _, one = coroutine.resume(co, 10)
local _, two = coroutine.resume(co, 5)
local _, three = coroutine.resume(co, 5)
print(string.format("demo: coroutine %d %d %d", one, two, three))

-- Ошибки: `error` и `pcall` — раскрутка через `longjmp`, то есть через нашу libc.
local ok, err = pcall(function() error("intended failure") end)
-- Само сообщение — после последнего двоеточия: перед ним Lua ставит место, где
-- случилась ошибка, а имя файла у него тоже с двоеточием.
local message = err:match("[^:]*$"):gsub("^%s+", "")
print(string.format("demo: pcall returned %s and said %s", tostring(ok), message))

-- Сборщик мусора: сто тысяч таблиц, созданных и брошенных.
local before = collectgarbage("count")
for _ = 1, 100000 do
  local _ = { 1, 2, 3 }
end
collectgarbage("collect")
print(string.format("demo: gc kept %d KiB after 100000 tables (was %d)",
                    math.floor(collectgarbage("count")), math.floor(before)))

-- Файлы: запись, чтение, переименование, удаление. Здесь язык кончается и
-- начинается система — `fopen`, `rename` и `remove` идут в наши системные
-- вызовы через libc.
local path = where .. "/lua-demo.txt"
local moved = where .. "/lua-demo-moved.txt"
local file = assert(io.open(path, "w"))
for i = 1, 5 do
  file:write(string.format("line %d\n", i))
end
file:close()

local lines, bytes = 0, 0
for line in io.lines(path) do
  lines = lines + 1
  bytes = bytes + #line
end
print(string.format("demo: wrote and read back %d lines, %d bytes", lines, bytes))

assert(os.rename(path, moved))
local check = assert(io.open(moved, "r"))
local first = check:read("l")
check:close()
assert(os.remove(moved))
print(string.format("demo: renamed and removed, first line was '%s'", first))

-- Время. Секунды эпохи — из часов машины; ноль означал бы, что часов нет.
local now = os.time()
print(string.format("demo: clock says %d seconds, which is %s",
                    now, os.date("!%Y-%m-%d", now)))

-- Запуск другой программы. У нас нет `sh`, поэтому `os.execute` отдаёт строку
-- тому же разборщику команд, что и оболочка, — см. `system` в
-- libc/freeos/syscalls.c.
local started = os.execute("/bin/hello")
print("demo: os.execute returned " .. tostring(started))

print("demo: done")
