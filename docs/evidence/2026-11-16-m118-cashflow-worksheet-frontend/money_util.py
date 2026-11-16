"""把金额字符串按数值比较（后端 Decimal 序列化会掉尾零：'100.0000' / '0' 都可能）。"""


def money_equal(left, right):
    if left is None or right is None:
        return left is right
    return float(left) == float(right)
