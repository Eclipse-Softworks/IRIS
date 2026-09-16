package main

import "fmt"

func collatzLength(n int64) int64 {
	steps := int64(0)
	x := n
	for x != 1 {
		if x%2 == 0 {
			x /= 2
		} else {
			x = 3*x + 1
		}
		steps++
	}
	return steps
}

func main() {
	maxSteps := int64(0)
	maxN := int64(1)
	limit := int64(500000)
	for n := int64(1); n <= limit; n++ {
		steps := collatzLength(n)
		if steps > maxSteps {
			maxSteps = steps
			maxN = n
		}
	}
	fmt.Printf("%d\n%d\n", maxN, maxSteps)
}
