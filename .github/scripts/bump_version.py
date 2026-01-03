#!/usr/bin/env python3

import sys

def bump_version(version: str, bump: str) -> str:
    try:
        major, minor, patch = map(int, version.split("."))
    except ValueError:
        raise ValueError("Version must be in MAJOR.MINOR.PATCH format")

    bump = bump.lower()

    if bump == "major":
        major += 1
        minor = 0
        patch = 0
    elif bump == "minor":
        minor += 1
        patch = 0
    elif bump == "patch":
        patch += 1
    else:
        raise ValueError("Bump must be one of: major, minor, patch")

    return f"{major}.{minor}.{patch}"


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("Usage: bump_version.py <version> <major|minor|patch>")
        sys.exit(1)

    version = sys.argv[1]
    bump = sys.argv[2]

    try:
        new_version = bump_version(version, bump)
        print(new_version)
    except ValueError as e:
        print(f"Error: {e}")
        sys.exit(1)
