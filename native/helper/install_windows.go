//go:build windows

package main

import (
	"path/filepath"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

// Browsers read HKCU\Software\<vendor>\NativeMessagingHosts\<name> (default value = manifest path): per user, no administrator needed.
var registryBases = []string{
	`Software\Google\Chrome\NativeMessagingHosts`,
	`Software\Microsoft\Edge\NativeMessagingHosts`,
	`Software\BraveSoftware\Brave-Browser\NativeMessagingHosts`,
	`Software\Chromium\NativeMessagingHosts`,
	`Software\Vivaldi\NativeMessagingHosts`,
}

func registerSystem(binPath string, mf []byte) (int, error) {
	mfPath := filepath.Join(filepath.Dir(binPath), hostName+".json")
	if err := writeFile(mfPath, mf); err != nil {
		return 0, err
	}
	n := 0
	for _, base := range registryBases {
		k, _, err := registry.CreateKey(registry.CURRENT_USER, base+`\`+hostName, registry.SET_VALUE)
		if err != nil {
			return n, err
		}
		err = k.SetStringValue("", mfPath)
		k.Close()
		if err != nil {
			return n, err
		}
		n++
	}
	return n, nil
}

func unregisterSystem() int {
	n := 0
	for _, base := range registryBases {
		if registry.DeleteKey(registry.CURRENT_USER, base+`\`+hostName) == nil {
			n++
		}
	}
	return n
}

// A double-clicked console program opens a console window: drop it before showing the dialog.
func prepareInstallUI() {
	_, _, _ = windows.NewLazySystemDLL("kernel32.dll").NewProc("FreeConsole").Call()
}

func showDialog(title, text string, ok bool) {
	flags := uint32(windows.MB_OK | windows.MB_ICONINFORMATION)
	if !ok {
		flags = windows.MB_OK | windows.MB_ICONERROR
	}
	t, _ := windows.UTF16PtrFromString(text)
	c, _ := windows.UTF16PtrFromString(title)
	_, _ = windows.MessageBox(0, t, c, flags)
}
