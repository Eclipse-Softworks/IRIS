def main():
    n = 256
    size = n * n
    a = [0.0] * size
    b = [0.0] * size
    c = [0.0] * size

    for i in range(size):
        row = i // n
        col = i % n
        a[i] = ((row * col) % 13) * 0.1
        b[i] = ((row + col) % 17) * 0.1

    for r in range(n):
        for col in range(n):
            s = 0.0
            for k in range(n):
                s += a[r * n + k] * b[k * n + col]
            c[r * n + col] = s

    print(int(c[0]))
    print(int(c[size - 1]))

if __name__ == "__main__":
    main()
