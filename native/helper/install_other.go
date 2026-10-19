//go:build !windows

package main

import (
	"os/exec"
	"runtime"
	"strings"
)

func registerSystem(string, []byte) (int, error) { return 0, nil } // per-user manifests only (see browserHostDirs)
func unregisterSystem() int                      { return 0 }
func prepareInstallUI()                          {}

// macOS: a dialog through AppleScript so a double-click from Finder shows something; elsewhere the text on stdout is all there is.
func showDialog(title, text string, ok bool) {
	if runtime.GOOS != "darwin" {
		return
	}
	esc := func(s string) string { return strings.ReplaceAll(strings.ReplaceAll(s, `\`, `\\`), `"`, `\"`) }
	icon := "note"
	if !ok {
		icon = "stop"
	}
	_ = exec.Command("osascript", "-e", `display dialog "`+esc(text)+`" with title "`+esc(title)+`" buttons {"OK"} default button "OK" with icon `+icon).Run()
}
