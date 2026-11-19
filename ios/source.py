#!/usr/bin/env python3
"""Prints an AltStore/SideStore source listing one Roamlog build.

Usage: ios/source.py IPA VERSION BUILD DOWNLOAD_URL SOURCE_URL ICON_URL
Users add SOURCE_URL in SideStore once; each publish replaces it with the newest build.
"""
import datetime
import hashlib
import json
import os
import sys

ipa, version, build, download_url, source_url, icon_url = sys.argv[1:7]
with open(ipa, "rb") as file:
    digest = hashlib.sha256(file.read()).hexdigest()

print(json.dumps({
    "name": "Roamlog",
    "identifier": "dev.traveler.source",
    "sourceURL": source_url,
    "iconURL": icon_url,
    "apps": [{
        "name": "Roamlog",
        "bundleIdentifier": "dev.traveler",
        "developerName": "Roamlog",
        "subtitle": "Record trips and explore roads you have never traveled.",
        "localizedDescription": (
            "Local-first trip recording with road coverage, offline maps and "
            "encrypted phone-to-phone location sharing. Nightly build."
        ),
        "iconURL": icon_url,
        "tintColor": "2F6FED",
        "category": "navigation",
        "versions": [{
            "version": version,
            "buildVersion": build,
            "date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "localizedDescription": f"Nightly build {build}.",
            "downloadURL": download_url,
            "size": os.path.getsize(ipa),
            "sha256": digest,
            "minOSVersion": "17.0",
        }],
        "appPermissions": {
            "entitlements": [],
            "privacy": {
                "NSLocationWhenInUseUsageDescription":
                    "Roamlog records your trips and shows where you are on the map.",
            },
        },
    }],
    "news": [],
}, indent=2))
