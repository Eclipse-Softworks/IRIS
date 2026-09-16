package main

import "fmt"

func main() {
	n := 256
	size := n * n
	a := make([]float64, size)
	b := make([]float64, size)
	c := make([]float64, size)

	for i := 0; i < size; i++ {
		row := i / n
		col := i % n
		a[i] = float64((row*col)%13) * 0.1
		b[i] = float64((row+col)%17) * 0.1
	}

	for r := 0; r < n; r++ {
		for col := 0; col < n; col++ {
			sum := 0.0
			for k := 0; k < n; k++ {
				sum += a[r*n+k] * b[k*n+col]
			}
			c[r*n+col] = sum
		}
	}

	fmt.Printf("%d\n%d\n", int64(c[0]), int64(c[size-1]))
}
