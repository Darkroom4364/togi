#include <assert.h>

int is_big(int x);

int main(void) {
    assert(!is_big(2));
    assert(is_big(3));
    return 0;
}
