/* Стандартная библиотека C++ (фаза 70d): libc++, libc++abi, libunwind.
 *
 * # Что она доказывает
 *
 * Что программа на C++ — с исключениями, RTTI, потоками и контейнерами,
 * написанная без единой строчки под FossAi, — работает. Это ровно то, на чём
 * стоит llama.cpp, и каждая проверка ловит свою поломку:
 *
 * - исключение летит через три кадра и функцию без `catch` — без таблиц
 *   раскрутки (`freeos-cxx.ld`) или с потерянным `.gcc_except_table` программа
 *   упала бы в `std::terminate`;
 * - деструкторы объектов на пути исключения зовутся — счётчик это видит;
 * - `dynamic_cast` и `typeid` — RTTI;
 * - статический объект построен до `main` (`.init_array`);
 * - четыре `std::thread` под `std::mutex` считают **неатомарный** счётчик, а
 *   `thread_local` у каждого свой;
 * - `std::condition_variable::wait_for` возвращается по сроку не раньше него —
 *   по `steady_clock`;
 * - `std::regex`, `std::map`, `std::unordered_map`, `std::to_chars`/
 *   `std::from_chars` с плавающей точкой, `std::ostringstream`;
 * - исключение, брошенное в потоке, ловится в нём же, а `std::exception_ptr`
 *   переносит его в главный поток.
 *
 * Итог — одна строка `cxx: N failed`; ноль и только ноль — всё прошло.
 */

#include <atomic>
#include <charconv>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <exception>
#include <iostream>
#include <map>
#include <memory>
#include <mutex>
#include <regex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <typeinfo>
#include <unordered_map>
#include <vector>

static int failed = 0;

static void check(bool ok, const char *what) {
    std::printf("cxx: %-28s %s\n", what, ok ? "ok" : "FAILED");
    if (!ok) {
        failed++;
    }
}

/* Построен до `main`: без `.init_array` поле осталось бы нулём. */
struct Early {
    int value;
    Early() : value(42) {}
};
static Early early;

static int destroyed = 0;
struct Guard {
    ~Guard() { destroyed++; }
};

[[gnu::noinline]] static void deepest(int depth) {
    Guard guard;
    if (depth == 0) {
        throw std::runtime_error("from the deep");
    }
    deepest(depth - 1);
}

[[gnu::noinline]] static void no_catch_here() {
    Guard guard;
    deepest(2);
}

struct Base {
    virtual ~Base() = default;
};
struct Derived : Base {
    int tag = 7;
};

static thread_local int own = 0;

int main() {
    check(early.value == 42, "static constructor");

    std::string message;
    try {
        no_catch_here();
    } catch (const std::exception &error) {
        message = error.what();
    }
    check(message == "from the deep", "exception through frames");
    check(destroyed == 4, "destructors while unwinding");

    bool caught_int = false;
    try {
        throw 5;
    } catch (int value) {
        caught_int = value == 5;
    }
    check(caught_int, "throw int");

    std::unique_ptr<Base> object = std::make_unique<Derived>();
    Derived *derived = dynamic_cast<Derived *>(object.get());
    check(derived != nullptr && derived->tag == 7, "dynamic_cast");
    check(typeid(*object) == typeid(Derived), "typeid");

    std::mutex lock;
    long counter = 0;
    std::atomic<int> distinct{0};
    std::vector<std::thread> threads;
    for (int index = 1; index <= 4; index++) {
        threads.emplace_back([&, index] {
            own = index;
            for (int step = 0; step < 100000; step++) {
                std::lock_guard<std::mutex> hold(lock);
                counter++;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
            if (own == index) {
                distinct++;
            }
        });
    }
    for (std::thread &thread : threads) {
        thread.join();
    }
    check(counter == 400000, "std::thread + std::mutex");
    check(distinct == 4 && own == 0, "thread_local");

    std::condition_variable wake;
    std::unique_lock<std::mutex> hold(lock);
    auto start = std::chrono::steady_clock::now();
    std::cv_status status = wake.wait_for(hold, std::chrono::milliseconds(50));
    auto waited = std::chrono::steady_clock::now() - start;
    hold.unlock();
    check(status == std::cv_status::timeout && waited >= std::chrono::milliseconds(50), "wait_for timeout");

    std::exception_ptr carried;
    std::thread thrower([&] {
        try {
            throw std::logic_error("from a thread");
        } catch (...) {
            carried = std::current_exception();
        }
    });
    thrower.join();
    std::string carried_message;
    try {
        if (carried) {
            std::rethrow_exception(carried);
        }
    } catch (const std::logic_error &error) {
        carried_message = error.what();
    }
    check(carried_message == "from a thread", "exception_ptr across threads");

    std::regex pattern("([a-z]+)-([0-9]+)");
    std::smatch match;
    std::string text = "llama-70";
    check(std::regex_match(text, match, pattern) && match[1] == "llama" && match[2] == "70", "std::regex");

    std::map<std::string, int> ordered{{"b", 2}, {"a", 1}, {"c", 3}};
    std::unordered_map<int, std::string> hashed;
    for (int index = 0; index < 1000; index++) {
        hashed[index] = std::to_string(index * index);
    }
    check(ordered.begin()->first == "a" && hashed[31] == "961", "map / unordered_map");

    char buffer[32];
    auto written = std::to_chars(buffer, buffer + sizeof buffer, 3.25);
    double parsed = 0;
    std::from_chars(buffer, written.ptr, parsed);
    check(std::string(buffer, written.ptr) == "3.25" && parsed == 3.25, "to_chars / from_chars");

    std::ostringstream stream;
    stream << "pi=" << 3.5 << " n=" << 12;
    check(stream.str() == "pi=3.5 n=12", "ostringstream");

    std::cout << "cxx: iostream says hello" << std::endl;
    std::printf("cxx: %d failed\n", failed);
    return failed == 0 ? 0 : 1;
}
