# Offline generator for the RS256 and PS256 test vectors. The private keys exist only in this process and are
# thrown away; only the public JWKS and the signed tokens are written.
import base64, json, sys
from cryptography.hazmat.primitives.asymmetric import rsa, padding
from cryptography.hazmat.primitives import hashes

def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
def jwk(pub, kid, alg):
    n = pub.public_numbers()
    ln = lambda i: i.to_bytes((i.bit_length() + 7) // 8, "big")
    return {"kty": "RSA", "kid": kid, "alg": alg, "use": "sig", "n": b64u(ln(n.n)), "e": b64u(ln(n.e))}

ISS, AUD = "https://idp.test/api/auth", "https://app.test/mcp"
CLAIMS = {"iss": ISS, "sub": "u1", "aud": [AUD, ISS + "/oauth2/userinfo"], "exp": 4102444800, "iat": 1700000000}

def token(alg, key, kid, claims):
    h = b64u(json.dumps({"alg": alg, "typ": "at+jwt", "kid": kid}, separators=(",", ":")).encode())
    p = b64u(json.dumps(claims, separators=(",", ":")).encode())
    msg = f"{h}.{p}".encode()
    if alg == "RS256": sig = key.sign(msg, padding.PKCS1v15(), hashes.SHA256())
    else: sig = key.sign(msg, padding.PSS(mgf=padding.MGF1(hashes.SHA256()), salt_length=32), hashes.SHA256())
    return f"{h}.{p}.{b64u(sig)}"

good = rsa.generate_private_key(public_exponent=65537, key_size=2048)
other = rsa.generate_private_key(public_exponent=65537, key_size=2048)
rs = token("RS256", good, "rsa1", CLAIMS)
h, p, s = rs.split(".")
forged_payload = b64u(json.dumps({**CLAIMS, "sub": "admin"}, separators=(",", ":")).encode())
doc = {
  "note": "RS256 and PS256 vectors made once by gen-jwt-vectors.py with throwaway 2048-bit keys; only public data is kept. Tokens expire in 2100, so tests pass their own clock (now).",
  "now": 1795000000, "issuer": ISS, "audience": AUD,
  "jwks": {"keys": [jwk(good.public_key(), "rsa1", "RS256"), jwk(good.public_key(), "rsa1", "PS256")]},
  "tokens": {
    "rs256": rs,
    "ps256": token("PS256", good, "rsa1", CLAIMS),
    "rs256_tampered_payload": f"{h}.{forged_payload}.{s}",
    "rs256_other_key": token("RS256", other, "rsa1", CLAIMS),
  },
}
json.dump(doc, sys.stdout, indent=2); print()
