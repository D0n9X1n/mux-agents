# Icon tests: the mark in assets/icon, its PNG sizes, and the README header. Untagged: they run on every OS.
$repoRoot = Split-Path -Parent $PSScriptRoot
$iconRoot = [IO.Path]::Combine($repoRoot, 'assets', 'icon')

Describe 'MuxAgents icon' {
    It 'has the full mark and a 16 px mark as SVG, each a titled square with no script or outside reference' {
        $result = foreach ($pair in @(@('mux-agents.svg', '0 0 512 512'), @('mux-agents-16.svg', '0 0 16 16'))) {
            $path = Join-Path $iconRoot $pair[0]
            if (-not (Test-Path -LiteralPath $path)) { "$($pair[0]) missing"; continue }
            $doc = [xml][IO.File]::ReadAllText($path)
            $svg = $doc.DocumentElement
            $square = $svg.viewBox -ceq $pair[1]
            $titled = $svg.title -ceq 'mux-agents'
            # Walk the parsed document, not its text: only drawing elements in the SVG namespace, no doctype or
            # processing instruction, no href or on* attribute, and url() only to an id inside the file.
            $drawing = 'svg', 'title', 'defs', 'linearGradient', 'stop', 'rect', 'path', 'circle'
            $closed = $null -eq $doc.DocumentType
            foreach ($node in $doc.SelectNodes('//node()')) {
                if ($node.NodeType -in 'Text', 'Whitespace', 'SignificantWhitespace', 'Comment') { continue }
                if ($node.NodeType -ne 'Element' -or $node.NamespaceURI -cne 'http://www.w3.org/2000/svg' -or $node.LocalName -cnotin $drawing) { $closed = $false; continue }
                foreach ($attribute in $node.Attributes) {
                    if ($attribute.LocalName -ieq 'href' -or $attribute.LocalName -imatch '^on' -or $attribute.Value -imatch 'url\(\s*(?!#)') { $closed = $false }
                }
            }
            '{0} {1} {2} {3} {4}' -f $pair[0], $svg.LocalName, $square, $titled, $closed
        }
        $result -join ' ; ' | Should BeExactly 'mux-agents.svg svg True True True ; mux-agents-16.svg svg True True True'
    }
    It 'has a PNG at 16, 32, 64, 128, 256 and 512 px, each that size' {
        $result = foreach ($size in 16, 32, 64, 128, 256, 512) {
            $path = Join-Path $iconRoot "mux-agents-$size.png"
            if (-not (Test-Path -LiteralPath $path)) { "$size missing"; continue }
            $bytes = [IO.File]::ReadAllBytes($path)
            $signature = [BitConverter]::ToString($bytes, 0, 8)
            # IHDR width and height, big-endian, at bytes 16 and 20. Each byte is widened first: a shifted [byte] stays a byte.
            $width = ([int]$bytes[16] -shl 24) -bor ([int]$bytes[17] -shl 16) -bor ([int]$bytes[18] -shl 8) -bor [int]$bytes[19]
            $height = ([int]$bytes[20] -shl 24) -bor ([int]$bytes[21] -shl 16) -bor ([int]$bytes[22] -shl 8) -bor [int]$bytes[23]
            "$size=$($signature -ceq '89-50-4E-47-0D-0A-1A-0A'),${width}x$height"
        }
        $result -join ' ' | Should BeExactly '16=True,16x16 32=True,32x32 64=True,64x64 128=True,128x128 256=True,256x256 512=True,512x512'
    }
    It 'README opens with the icon above the title' {
        $lines = [IO.File]::ReadAllLines((Join-Path $repoRoot 'README.md'))
        "$($lines[0]) | $($lines[2])" | Should BeExactly '<p align="center"><img src="assets/icon/mux-agents.svg" width="128" height="128" alt="mux-agents icon"></p> | # mux-agents'
    }
    It 'keeps assets out of the plugin zip' {
        [IO.File]::ReadAllText((Join-Path $repoRoot '.gitattributes')) | Should Match '(?m)^assets export-ignore$'
    }
}
