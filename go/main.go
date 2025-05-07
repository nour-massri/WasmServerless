package main

import (
	"fmt"
)

//export run
func run() int32 {
	fmt.Println("hello world")
	return 0
}

func main() {
	// This function is required for proper WASI initialization
	// It will call your exported "run" function
	run()
}
