#include <cassert>

bool is_big(int x);

int main() {
    assert(!is_big(2));
    assert(is_big(3));
    return 0;
}
