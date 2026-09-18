package main

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"flag"
	"fmt"
	"math"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"syscall"

	"github.com/UberMorgott/wartales-mp/internal/applog"
	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/install"
	"github.com/UberMorgott/wartales-mp/internal/master"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/relay"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge"
)

func runCmd(args []string) error {
	fs := flag.NewFlagSet("run", flag.ContinueOnError)
	port := fs.Int("port", 14250, "public TCP port for the relay and the proxy-link")
	masterAddr := fs.String("master", "127.0.0.1:60442", "master listen address")
	noWatch := fs.Bool("no-watch", false, "keep serving after the game exits")
	transport := fs.String("transport", master.ModeAuto, "game transport for lobbies we host: auto, direct or sdr")
	if err := fs.Parse(args); err != nil {
		return err
	}
	switch *transport {
	case master.ModeAuto, master.ModeDirect, master.ModeSDR:
	default:
		return fmt.Errorf("-transport must be auto, direct or sdr, not %q", *transport)
	}

	logger, closeLog := applog.Open(os.Stdout)
	defer func() { _ = closeLog() }() // shutting down; a failed close changes nothing
	logger.Printf("wartales-mp starting: port %d, master %s", *port, *masterAddr)

	// The winmm.dll proxy starts us from inside the game, so the certificates
	// have to be in place without anyone running a setup step first.
	dir := install.DataDir()
	if err := install.EnsureCerts(dir); err != nil {
		return fmt.Errorf("certificates: %w", err)
	}
	tlsCfg, err := install.TLSConfig(dir)
	if err != nil {
		return err
	}

	// Attach to the game before opening any port: if it is already gone there
	// is nothing to serve.
	// A nil channel blocks forever, which is exactly what -no-watch means.
	var done <-chan struct{}
	if !*noWatch {
		pid, exited, err := watchGame()
		if err != nil {
			return err
		}
		logger.Printf("attached to %s, pid %d", gameExe, pid)
		done = exited
	}

	rl := relay.New(logger)
	mapper := nat.NewMapper(*port, logger)
	defer mapper.Close()

	// The shim's SDR bridge: its verdict and port live in sdr.status next to
	// our log. The bridge follows that file for as long as we run, because
	// the game's Steam API comes up after us.
	bridge := sdrbridge.New(filepath.Join(applog.Dir(), "sdr.status"), logger)
	var key [4]byte
	_, _ = rand.Read(key[:]) // crypto/rand.Read never returns an error

	ms := master.New(master.Options{
		Addr:       *masterAddr,
		TLS:        tlsCfg,
		RelayPort:  *port,
		HostPW:     rl.HostPW,
		SlavePW:    rl.SlavePW,
		Log:        logger,
		PublicAddr: mapper.Addr,
		Transport:  *transport,
		Endpoint:   mapper.Endpoint,
		SDRStatus:  bridge.Status,
		Bridge:     bridge,
		LinkKey:    binary.BigEndian.Uint32(key[:]),
	})
	bridge.OnPeer = ms.ServeSDRLink
	logger.Printf("transport mode %s (direct relay when the public endpoint is verified, else SDR for everything)", *transport)
	defer ms.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go bridge.Run(ctx)

	errs := make(chan error, 2)
	go func() { errs <- rl.ListenAndServe(fmt.Sprintf(":%d", *port), ms.ServeLink) }()
	go func() { errs <- ms.ListenAndServe() }()

	// Warm the public endpoint up so the first join code is instant.
	go func() {
		ep, err := mapper.Endpoint()
		switch {
		case err != nil:
			logger.Printf("public endpoint unknown: %v", err)
		case ep.Reachable:
			logger.Printf("public endpoint: %s (via %s)", ep.Addr, ep.Source)
		default:
			// Double NAT and friends: the code still works on the LAN, but
			// nobody on the internet can reach it - say so instead of
			// handing out a silently broken code.
			logger.Printf("WARNING: %s", ep.Warning)
			logger.Printf("LAN-only endpoint: %s (via %s)", ep.Addr, ep.Source)
		}
	}()

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
		if port < 0 || port > math.MaxUint16 {
			return fmt.Errorf("port %d is out of range", port)
		}
		s, err := code.Encode(code.Endpoint{IP: ip, Port: uint16(port)})
		if err != nil {
			return err
		}
		fmt.Println(s)
	case "decode":
		c, err := code.DecodeAny(args[1])
		if err != nil {
			return err
		}
		if c.Steam != nil {
			fmt.Printf("steam %d key %08x (flags %d)\n", c.Steam.SteamID64(), c.Steam.Key, c.Steam.Flags)
		} else {
			fmt.Printf("%s (flags %d)\n", c.Endpoint.Addr(), c.Endpoint.Flags)
		}
	default:
		return fmt.Errorf("usage: wartales-mp code encode IP:PORT | code decode CODE")
	}
	return nil
}
