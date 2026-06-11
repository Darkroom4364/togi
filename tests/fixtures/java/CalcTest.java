public final class CalcTest {
    public static void main(String[] args) {
        assertFalse(Calc.isBig(2));
        assertTrue(Calc.isBig(3));
    }

    private static void assertTrue(boolean value) {
        if (!value) {
            throw new AssertionError("expected true");
        }
    }

    private static void assertFalse(boolean value) {
        if (value) {
            throw new AssertionError("expected false");
        }
    }
}
