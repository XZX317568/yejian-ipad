"""Package Xcode's iPad device build as a sideloadable IPA."""

import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
from tempfile import TemporaryDirectory
from zipfile import ZipFile


def package(app: Path, output: Path) -> Path:
    with (app / "Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if (info.get("CFBundleIdentifier") != "app.yejian.reader.rust"
            or "iPhoneOS" not in info.get("CFBundleSupportedPlatforms", [])
            or 2 not in info.get("UIDeviceFamily", [])):
        raise ValueError("请使用支持 iPad 真机的页间 Xcode 构建。")
    executable = info["CFBundleExecutable"]
    if Path(executable).name != executable or not (app / executable).is_file():
        raise ValueError("应用内缺少主程序。")
    version = info["CFBundleShortVersionString"]
    build = info["CFBundleVersion"]
    if not all(re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", value) for value in (version, build)):
        raise ValueError("应用版本号或构建号无效。")

    output.mkdir(parents=True, exist_ok=True)
    filename = f"Yejian-{version}-{build}-ipad-unsigned.ipa"
    with TemporaryDirectory(dir=output) as scratch:
        payload = Path(scratch) / "Payload"
        payload.mkdir()
        shutil.copytree(app, payload / app.name, symlinks=True)
        staged = Path(scratch) / filename
        subprocess.run(["tar", "-c", "--format", "zip", "--no-mac-metadata",
                        "-f", str(staged.resolve()), "-C", scratch, "Payload"], check=True)
        with ZipFile(staged) as ipa:
            prefix = f"Payload/{app.name}/"
            binary = ipa.getinfo(prefix + executable)
            if not (binary.external_attr >> 16) & 0o111 or not binary.file_size:
                raise ValueError("IPA 中的主程序缺少内容或可执行权限。")
            if plistlib.loads(ipa.read(prefix + "Info.plist")) != info:
                raise ValueError("IPA 内的应用信息与构建结果不符。")
        destination = output / filename
        os.replace(staged, destination)
    return destination


if __name__ == "__main__":
    print(package(Path(sys.argv[1]), Path(sys.argv[2])))
