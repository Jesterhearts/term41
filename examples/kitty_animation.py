"""Upload a Kitty animation and let the terminal play three loops after exit."""

import base64
import secrets
import sys


def send(keys: str, data: bytes = b"") -> None:
    encoded = base64.b64encode(data)
    chunks = [encoded[i : i + 4096] for i in range(0, len(encoded), 4096)] or [b""]
    for index, chunk in enumerate(chunks):
        control = keys if index == 0 else ("a=f" if keys.startswith("a=f,") else "")
        prefix = f"{control}," if control else ""
        more = int(index + 1 < len(chunks))
        sys.stdout.buffer.write(
            f"\x1b_G{prefix}q=2,m={more};".encode() + chunk + b"\x1b\\"
        )


def main() -> None:
    image_id = secrets.randbelow(2**32 - 1) + 1
    send(f"a=t,i={image_id},f=32,s=64,v=24", bytes([22, 28, 48, 255]) * (64 * 24))
    positions = list(range(0, 57, 4)) + list(range(52, 0, -4))
    for x in positions:
        send(
            f"a=f,i={image_id},f=32,s=6,v=6,c=1,x={x},y=9,z=60",
            bytes([80, 220, 170, 255]) * 36,
        )
    send(f"a=p,i={image_id},c=16,r=3")
    send(f"a=a,i={image_id},s=3,v=4")
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
