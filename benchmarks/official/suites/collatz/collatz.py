def collatz_length(n: int) -> int:
    steps = 0
    x = n
    while x != 1:
        if x % 2 == 0:
            x //= 2
        else:
            x = 3 * x + 1
        steps += 1
    return steps

def main():
    max_steps = 0
    max_n = 1
    limit = 500000
    for n in range(1, limit + 1):
        steps = collatz_length(n)
        if steps > max_steps:
            max_steps = steps
            max_n = n
    print(max_n)
    print(max_steps)

if __name__ == "__main__":
    main()
