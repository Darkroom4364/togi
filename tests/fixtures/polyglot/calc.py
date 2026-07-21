def is_adult(age: int) -> bool:
    return age >= 18


def discount(total: float) -> float:
    return total * 0.9 if total > 100 else total
