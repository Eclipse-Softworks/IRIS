package main

import "fmt"

func partition(arr []int64, low, high int) int {
	pivot := arr[high]
	i := low - 1
	for j := low; j < high; j++ {
		if arr[j] <= pivot {
			i++
			arr[i], arr[j] = arr[j], arr[i]
		}
	}
	arr[i+1], arr[high] = arr[high], arr[i+1]
	return i + 1
}

func quicksort(arr []int64, low, high int) {
	if low < high {
		pi := partition(arr, low, high)
		quicksort(arr, low, pi-1)
		quicksort(arr, pi+1, high)
	}
}

func main() {
	n := 100000
	arr := make([]int64, n)
	seed := int64(123456789)
	for i := 0; i < n; i++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		arr[i] = seed
	}

	quicksort(arr, 0, n-1)

	fmt.Printf("%d\n%d\n", arr[0], arr[n-1])
}
