public static class CalcTest
{
    public static void Main()
    {
        AssertFalse(Calc.IsBig(2));
        AssertTrue(Calc.IsBig(3));
    }

    private static void AssertTrue(bool value)
    {
        if (!value)
        {
            throw new System.Exception("expected true");
        }
    }

    private static void AssertFalse(bool value)
    {
        if (value)
        {
            throw new System.Exception("expected false");
        }
    }
}
