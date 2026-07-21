import unittest

from calc import discount, is_adult


class TestCalc(unittest.TestCase):
    # Deliberately weak: only the boundary case, discount() never tested.
    def test_is_adult_boundary(self):
        self.assertTrue(is_adult(18))


if __name__ == "__main__":
    unittest.main()
