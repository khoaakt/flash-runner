// Self-installation: run without Chrome's arguments (a double-click, or `install`) the helper copies itself to a per-user location and
// registers the native messaging manifest with every Chromium-family browser it finds. `uninstall` reverses it. `--dir <d>` registers
// into one NativeMessagingHosts directory instead (tests, unusual browsers); `--quiet` suppresses the dialog.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

type manifest struct {
	Name           string   `json:"name"`
	Description    string   `json:"description"`
	Path           string   `json:"path"`
	Type           string   `json:"type"`
	AllowedOrigins []string `json:"allowed_origins"`
}

func writeFile(path string, b []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(path, b, 0o644)
}

func manifestJSON(path string) []byte {
	b, _ := json.MarshalIndent(manifest{hostName, "Raw TCP sockets for the flash-runner extension", path, "stdio", []string{extOrigin}}, "", "  ")
	return append(b, '\n')
}

type installOpts struct {
	dirs  []string // NativeMessagingHosts directories (nil: every browser profile found)
	quiet bool
}

func parseInstallArgs(args []string) installOpts {
	var o installOpts
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--dir":
			if i+1 < len(args) {
				o.dirs = append(o.dirs, args[i+1])
				i++
			}
		case "--quiet":
			o.quiet = true
		}
	}
	return o
}

// copySelf puts the running binary at dest (unless it already runs from there) and returns dest.
func copySelf(dest string) (string, error) {
	self, err := os.Executable()
	if err != nil {
		return "", err
	}
	if self, err = filepath.EvalSymlinks(self); err != nil {
		return "", err
	}
	if abs, _ := filepath.Abs(dest); abs == self {
		return self, nil
	}
	if err := os.MkdirAll(filepath.Dir(dest), 0o755); err != nil {
		return "", err
	}
	in, err := os.Open(self)
	if err != nil {
		return "", err
	}
	defer in.Close()
	tmp := dest + ".new"
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o755)
	if err != nil {
		return "", err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return "", err
	}
	if err := out.Close(); err != nil {
		return "", err
	}
	if err := os.Rename(tmp, dest); err != nil {
		return "", err
	}
	return dest, nil
}

// Per-user profile roots of the browsers that support native messaging, for the non-Windows platforms (Windows uses the registry).
func browserHostDirs() []string {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	var roots []string
	if runtime.GOOS == "darwin" {
		b := filepath.Join(home, "Library", "Application Support")
		for _, p := range []string{"Google/Chrome", "Google/Chrome Beta", "Google/Chrome Canary", "Chromium", "BraveSoftware/Brave-Browser", "Microsoft Edge", "Vivaldi", "Arc/User Data"} {
			roots = append(roots, filepath.Join(b, filepath.FromSlash(p)))
		}
	} else {
		b := filepath.Join(home, ".config")
		for _, p := range []string{"google-chrome", "google-chrome-beta", "chromium", "BraveSoftware/Brave-Browser", "microsoft-edge", "vivaldi"} {
			roots = append(roots, filepath.Join(b, filepath.FromSlash(p)))
		}
	}
	var dirs []string
	for _, r := range roots {
		if st, err := os.Stat(r); err == nil && st.IsDir() {
			dirs = append(dirs, filepath.Join(r, "NativeMessagingHosts"))
		}
	}
	return dirs
}

func defaultInstallDir() string {
	home, _ := os.UserHomeDir()
	switch runtime.GOOS {
	case "darwin":
		return filepath.Join(home, "Library", "Application Support", "FlashRunnerHelper")
	case "windows":
		if d := os.Getenv("LOCALAPPDATA"); d != "" {
			return filepath.Join(d, "FlashRunnerHelper")
		}
		return filepath.Join(home, "AppData", "Local", "FlashRunnerHelper")
	default:
		return filepath.Join(home, ".local", "share", "flash-helper")
	}
}

func runInstall(install bool, args []string) int {
	o := parseInstallArgs(args)
	prepareInstallUI() // Windows: no console window for a double-click
	var lines []string
	fail := func(err error) int {
		lines = append(lines, "Could not install the helper: "+err.Error())
		report(lines, false, o.quiet)
		return 1
	}
	binName := "flash-helper"
	if runtime.GOOS == "windows" {
		binName += ".exe"
	}
	dest := filepath.Join(defaultInstallDir(), binName)
	if !install {
		n := 0
		if o.dirs == nil {
			n += unregisterSystem()
			o.dirs = browserHostDirs()
		}
		for _, d := range o.dirs {
			if os.Remove(filepath.Join(d, hostName+".json")) == nil {
				n++
			}
		}
		_ = os.Remove(dest)
		_ = os.Remove(filepath.Join(filepath.Dir(dest), hostName+".json"))
		_ = os.Remove(filepath.Dir(dest))
		lines = append(lines, fmt.Sprintf("Helper removed (%d browser registration(s) deleted).", n))
		report(lines, true, o.quiet)
		return 0
	}
	path, err := copySelf(dest)
	if err != nil {
		return fail(err)
	}
	mf := manifestJSON(path)
	registered := 0
	if o.dirs == nil {
		n, err := registerSystem(path, mf) // Windows registry; a no-op elsewhere
		if err != nil {
			return fail(err)
		}
		registered += n
		o.dirs = browserHostDirs()
	}
	for _, d := range o.dirs {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return fail(err)
		}
		if err := os.WriteFile(filepath.Join(d, hostName+".json"), mf, 0o644); err != nil {
			return fail(err)
		}
		registered++
	}
	if registered == 0 {
		lines = append(lines, "No Chrome, Edge, Brave or Chromium profile was found on this computer. Start the browser once, then run the installer again.")
		report(lines, false, o.quiet)
		return 1
	}
	lines = append(lines, "The Flash helper is installed.", "Go back to the extension's settings page: it will show \"Installed\" by itself.")
	report(lines, true, o.quiet)
	return 0
}

func report(lines []string, ok bool, quiet bool) {
	text := strings.Join(lines, "\n")
	fmt.Println(text)
	if !quiet {
		showDialog("flash-runner — helper", text, ok)
	}
}
