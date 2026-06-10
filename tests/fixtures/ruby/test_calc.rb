require "minitest/autorun"
require_relative "calc"

class CalcTest < Minitest::Test
  def test_boundary
    refute is_big(2)
    assert is_big(3)
  end
end
