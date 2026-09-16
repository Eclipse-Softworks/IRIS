import sys
sys.setrecursionlimit(200000)

def partition(arr, low, high):
    pivot = arr[high]
    i = low - 1
    for j in range(low, high):
        if arr[j] <= pivot:
            i += 1
            arr[i], arr[j] = arr[j], arr[i]
    arr[i + 1], arr[high] = arr[high], arr[i + 1]
    return i + 1

def quicksort(arr, low, high):
    while low < high:
        pi = partition(arr, low, high)
        if pi - low < high - pi:
            quicksort(arr, low, pi - 1)
            low = pi + 1
        else:
            quicksort(arr, pi + 1, high)
            high = pi - 1

def main():
    n = 100000
    arr = [0] * n
    seed = 123456789
    for i in range(n):
        seed = (seed * 1103515245 + 12345) % 2147483648
        arr[i] = seed

    quicksort(arr, 0, n - 1)

    print(arr[0])
    print(arr[n - 1])

if __name__ == "__main__":
    main()
