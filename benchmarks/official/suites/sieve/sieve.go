package main

import "fmt"

func main() {
	limit := 100000
	sieve := make([]int64, limit+1)
	for i := 0; i <= limit; i++ {
		sieve[i] = 1
	}

	for i := 2; i*i <= limit; i++ {
		if sieve[i] == 1 {
			for j := i * i; j <= limit; j += i {
				sieve[j] = 0
			}
		}
	}

	count := int64(0)
	for i := 2; i <= limit; i++ {
		if sieve[i] == 1 {
			count++
		}
	}
	fmt.Println(count)
}
