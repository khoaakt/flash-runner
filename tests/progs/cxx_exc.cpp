#include <cstdio>
#include <string>
#include <vector>
#include <map>
#include <stdexcept>
#include <algorithm>
struct Base { virtual ~Base() {} virtual int f() const { return 1; } };
struct Derived : Base { int f() const override { return 2; } };
static int thrower(int depth) { if (depth == 0) throw std::runtime_error("deep"); return thrower(depth - 1) + 1; }
int main() {
    try { thrower(20); } catch (const std::exception &e) { std::printf("caught: %s\n", e.what()); }
    std::map<std::string, int> m; for (int i = 0; i < 100; i++) m["k" + std::to_string(i % 17)] += i;
    std::vector<int> v; for (auto &kv : m) v.push_back(kv.second); std::sort(v.begin(), v.end());
    long s = 0; for (int x : v) s = s * 31 + x;
    Base *b = new Derived; std::printf("virtual=%d map=%zu hash=%ld\n", b->f(), m.size(), s); delete b;
    try { std::vector<int>().at(3); } catch (const std::out_of_range &) { std::puts("out_of_range"); }
    try { throw 42; } catch (int x) { std::printf("int %d\n", x); }
    return 0;
}
