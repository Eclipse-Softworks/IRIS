def main():
    limit = 100000
    sieve = [1] * (limit + 1)

    i = 2
    while i * i <= limit:
        if sieve[i] == 1:
            for j in range(i * i, limit + 1, i):
                sieve[j] = 0
        i += 1

    count = sum(1 for i in range(2, limit + 1) if sieve[i] == 1)
    print(count)

if __name__ == "__main__":
    main()
