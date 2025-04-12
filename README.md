# SilkyArcTool_rs

rust version of <https://github.com/TesterTesterov/SilkyArcTool>, with bug fixes and improvements.

Written by _Gemini 2.5 Pro Preview 03-25_.

## Features

- bug fix: packing unusable `voice.arc`.
- lzss compression packing is faster than original python code by using more CPU cores.
- compress-as-you-need.

## Usage

Install binary from [Release](https://github.com/lxl66566/SilkyArcTool-rs/releases), then runs

```sh
silkyarctool -h
```

to see help message.

## Tested on

- きまぐれテンプテーション (`voice.arc`)

## Tip

- Do not use `--compress` while packing voice.
