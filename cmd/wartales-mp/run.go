package main

import (
	"flag"
	"fmt"
	"log"
	"net"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"syscall"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/install"
	"github.com/UberMorgott/wartales-mp/internal/master"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/relay"
)

func runCmd(args []string) error {
	fs := flag.NewFlagSet("run", flag.ContinueOnError)
	port := fs.Int("port", 14250, "public TCP port for the relay and the proxy-link")
	masterAddr := fs.String("master", "127.0.0.1:60442", "master listen address")
	gamePath := fs.String("game", "", "path to Wartales.exe")
	noGame := fs.Bool("no-game", false, "do not launch the game")
	if err := fs.Parse(args); err != nil {
		return err
	}

	logger := log.New(os.Stdout, "", log.LstdFlags)

	dir := install.DataDir()
	tlsCfg, err := install.TLSConfig(dir)
	if err != nil {
		return err
	}

	rl := relay.New(logger)
	mapper := nat.NewMapper(*port, logger)
	defer mapper.Close()

	ms := master.New(master.Options{
		Addr:       *masterAddr,
		TLS:        tlsCfg,
		RelayPort:  *port,
		HostPW:     rl.HostPW,
		SlavePW:    rl.SlavePW,
		Log:        logger,
		PublicAddr: mapper.Addr,
	})
	defer ms.Close()

	errs := make(chan error, 2)
	go func() { errs <- rl.ListenAndServe(fmt.Sprintf(":%d", *port), ms.ServeLink) }()
	go func() { errs <- ms.ListenAndServe() }()

	// Warm the public endpoint up so the first join code is instant.
	go func() {
		if addr, err := mapper.Addr(); err == nil {
			logger.Printf("public endpoint: %s", addr)
		} else {
			logger.Printf("public endpoint unknown: %v", err)
		}
	}()

	done := make(chan error, 1)
	if !*noGame {
		exe := *gamePath
		if exe == "" {
			exe = findGame()
		}
		if exe == "" {
			return fmt.Errorf("could not find Wartales.exe, pass -game PATH")
		}
		logger.Printf("launching %s", exe)
		cmd := exec.Command(exe)
		cmd.Dir = filepath.Dir(exe)
		cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
		if err := cmd.Start(); err != nil {
			return err
		}
		go func() { done <- cmd.Wait() }()
	}

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM)

	select {
	case err := <-errs:
		return err
	case <-done:
		logger.Printf("game exited, shutting down")
		return nil
	case <-sig:
		logger.Printf("interrupted, shutting down")
		return nil
	}
}

// findGame looks in the usual Steam library locations.
func findGame() string {
	const rel = `steamapps\common\Wartales\Wartales.exe`
	roots := []string{
		`D:\Steam`,
		os.Getenv("ProgramFiles(x86)") + `\Steam`,
		os.Getenv("ProgramFiles") + `\Steam`,
		os.Getenv("SystemDrive") + `\Steam`,
	}
	candidates := append([]string{}, roots...)
	for _, r := range roots {
		candidates = append(candidates, libraryFolders(r)...)
	}
	for _, r := range candidates {
		if r == "" {
			continue
		}
		p := filepath.Join(r, rel)
		if st, err := os.Stat(p); err == nil && !st.IsDir() {
			return p
		}
	}
	return ""
}

var vdfPath = regexp.MustCompile(`"path"\s+"([^"]+)"`)

// libraryFolders reads the extra Steam libraries listed in libraryfolders.vdf.
func libraryFolders(steamRoot string) []string {
	if steamRoot == "" {
		return nil
	}
	raw, err := os.ReadFile(filepath.Join(steamRoot, "steamapps", "libraryfolders.vdf"))
	if err != nil {
		return nil
	}
	var out []string
	for _, m := range vdfPath.FindAllStringSubmatch(string(raw), -1) {
		out = append(out, strings.ReplaceAll(m[1], `\\`, `\`))
	}
	return out
}

func installCmd() error   { return install.Install(os.Stdout) }
func uninstallCmd() error { return install.Uninstall(os.Stdout) }

func codeCmd(args []string) error {
	if len(args) != 2 {
		return fmt.Errorf("usage: wartales-mp code encode IP:PORT | code decode CODE")
	}
	switch args[0] {
	case "encode":
		host, portStr, err := net.SplitHostPort(args[1])
		if err != nil {
			return err
		}
		port, err := strconv.Atoi(portStr)
		if err != nil {
			return err
		}
		ip := net.ParseIP(host)
		if ip == nil {
			return fmt.Errorf("%q is not an IP address", host)
		}
		s, err := code.Encode(code.Endpoint{IP: ip, Port: uint16(port)})
		if err != nil {
			return err
		}
		fmt.Println(s)
	case "decode":
		ep, err := code.Decode(args[1])
		if err != nil {
			return err
		}
		fmt.Printf("%s (flags %d)\n", ep.Addr(), ep.Flags)
	default:
		return fmt.Errorf("usage: wartales-mp code encode IP:PORT | code decode CODE")
	}
	return nil
}
