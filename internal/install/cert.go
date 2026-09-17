package install

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha1"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/hex"
	"encoding/pem"
	"fmt"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"time"
)

// Hosts the master certificate must be valid for. The game pins these names
// (sys.ssl.Socket sets the SNI/verify hostname to the configured host).
var MasterHosts = []string{"master.shirogames.com", "master2.shirogames.com"}

// File names inside the data directory.
const (
	caCertFile   = "ca.crt"
	caKeyFile    = "ca.key"
	leafCertFile = "master.crt"
	leafKeyFile  = "master.key"
)

// DataDir is where the generated certificates live.
func DataDir() string {
	base := os.Getenv("ProgramData")
	if base == "" {
		base = os.TempDir()
	}
	return filepath.Join(base, "wartales-mp")
}

// EnsureCerts generates the CA and the leaf certificate if they are missing.
// It is idempotent: existing, still valid files are reused.
func EnsureCerts(dir string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	if certValid(filepath.Join(dir, caCertFile)) && certValid(filepath.Join(dir, leafCertFile)) {
		return nil
	}

	caKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		return err
	}
	caTmpl := &x509.Certificate{
		SerialNumber:          serial(),
		Subject:               pkix.Name{CommonName: "wartales-mp local CA", Organization: []string{"wartales-mp"}},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().AddDate(10, 0, 0),
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageCRLSign | x509.KeyUsageDigitalSignature,
		BasicConstraintsValid: true,
		IsCA:                  true,
		MaxPathLen:            0,
	}
	caDER, err := x509.CreateCertificate(rand.Reader, caTmpl, caTmpl, &caKey.PublicKey, caKey)
	if err != nil {
		return err
	}
	caCert, err := x509.ParseCertificate(caDER)
	if err != nil {
		return err
	}

	leafKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		return err
	}
	leafTmpl := &x509.Certificate{
		SerialNumber: serial(),
		Subject:      pkix.Name{CommonName: MasterHosts[0], Organization: []string{"wartales-mp"}},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().AddDate(10, 0, 0),
		KeyUsage:     x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		DNSNames:     MasterHosts,
		IPAddresses:  []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
	}
	leafDER, err := x509.CreateCertificate(rand.Reader, leafTmpl, caCert, &leafKey.PublicKey, caKey)
	if err != nil {
		return err
	}

	if err := writePEM(filepath.Join(dir, caCertFile), "CERTIFICATE", caDER, 0o644); err != nil {
		return err
	}
	if err := writePEM(filepath.Join(dir, caKeyFile), "RSA PRIVATE KEY", x509.MarshalPKCS1PrivateKey(caKey), 0o600); err != nil {
		return err
	}
	if err := writePEM(filepath.Join(dir, leafCertFile), "CERTIFICATE", leafDER, 0o644); err != nil {
		return err
	}
	return writePEM(filepath.Join(dir, leafKeyFile), "RSA PRIVATE KEY", x509.MarshalPKCS1PrivateKey(leafKey), 0o600)
}

// TLSConfig loads the leaf certificate for the master listener.
func TLSConfig(dir string) (*tls.Config, error) {
	cert, err := tls.LoadX509KeyPair(filepath.Join(dir, leafCertFile), filepath.Join(dir, leafKeyFile))
	if err != nil {
		return nil, fmt.Errorf("%w (run \"wartales-mp install\" first)", err)
	}
	return &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS12}, nil
}

// CACertPath returns the path of the generated CA certificate.
func CACertPath(dir string) string { return filepath.Join(dir, caCertFile) }

// CAThumbprint returns the SHA-1 thumbprint certutil identifies the CA by.
func CAThumbprint(dir string) (string, error) {
	der, err := readCertDER(filepath.Join(dir, caCertFile))
	if err != nil {
		return "", err
	}
	sum := sha1.Sum(der)
	return hex.EncodeToString(sum[:]), nil
}

func serial() *big.Int {
	n, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 127))
	if err != nil {
		panic(err)
	}
	return n
}

func writePEM(path, blockType string, der []byte, mode os.FileMode) error {
	buf := pem.EncodeToMemory(&pem.Block{Type: blockType, Bytes: der})
	return os.WriteFile(path, buf, mode)
}

func readCertDER(path string) ([]byte, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	block, _ := pem.Decode(raw)
	if block == nil {
		return nil, fmt.Errorf("%s: not a PEM file", path)
	}
	return block.Bytes, nil
}

func certValid(path string) bool {
	der, err := readCertDER(path)
	if err != nil {
		return false
	}
	c, err := x509.ParseCertificate(der)
	if err != nil {
		return false
	}
	return time.Now().Before(c.NotAfter.Add(-24 * time.Hour))
}
