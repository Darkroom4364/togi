import unittest

from calc import is_big


class CalcTest(unittest.TestCase):
    def test_boundary(self) -> None:
        self.assertFalse(is_big(2))
        self.assertTrue(is_big(3))


if __name__ == "__main__":
    unittest.main()
