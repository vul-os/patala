module github.com/vul-os/patala/sdks/go

go 1.21

// The Go binding is ../../patala-go — this module holds examples of it, not a
// second copy of it. The replace keeps the examples building against the tree
// they live in, with no published module version to keep in step.
require github.com/vul-os/patala/patala-go v0.0.0

replace github.com/vul-os/patala/patala-go => ../../patala-go
