"""Writes a minimal, valid pure-Python wheel: make_wheel.py <outdir> <name> <version> <value>."""
import base64, hashlib, os, sys, zipfile

out, name, version, value = sys.argv[1:5]
mod = name.replace("-", "_")
dist = f"{mod}-{version}.dist-info"
files = {
    f"{mod}/__init__.py": f"VALUE = {value}\n".encode(),
    f"{dist}/METADATA": f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\nRequires-Python: >=3.8\n".encode(),
    f"{dist}/WHEEL": b"Wheel-Version: 1.0\nGenerator: pico2peta-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
}
record = ""
for p, b in files.items():
    d = base64.urlsafe_b64encode(hashlib.sha256(b).digest()).rstrip(b"=").decode()
    record += f"{p},sha256={d},{len(b)}\n"
record += f"{dist}/RECORD,,\n"
files[f"{dist}/RECORD"] = record.encode()
os.makedirs(out, exist_ok=True)
path = os.path.join(out, f"{mod}-{version}-py3-none-any.whl")
with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
    for p, b in files.items():
        z.writestr(p, b)
print(path)
