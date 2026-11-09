Add-Type -AssemblyName System.Drawing

function Create-ShieldIcon {
    $sizes = @(256, 64, 48, 32, 16)
    $pngs = @()

    foreach ($size in $sizes) {
        $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.Clear([System.Drawing.Color]::Transparent)

        $pad = [float]($size * 0.08)
        $w = [float]($size - 2 * $pad)
        $h = [float]($size - 2 * $pad)

        # Shield points
        $pts = [System.Drawing.PointF[]]@(
            (New-Object System.Drawing.PointF(($pad + $w * 0.5), $pad)),
            (New-Object System.Drawing.PointF(($pad + $w), ($pad + $h * 0.25))),
            (New-Object System.Drawing.PointF(($pad + $w * 0.85), ($pad + $h * 0.75))),
            (New-Object System.Drawing.PointF(($pad + $w * 0.5), ($pad + $h))),
            (New-Object System.Drawing.PointF(($pad + $w * 0.15), ($pad + $h * 0.75))),
            (New-Object System.Drawing.PointF($pad, ($pad + $h * 0.25)))
        )

        $rect = New-Object System.Drawing.RectangleF($pad, $pad, $w, $h)
        $c1 = [System.Drawing.Color]::FromArgb(255, 16, 185, 129) # Emerald Green
        $c2 = [System.Drawing.Color]::FromArgb(255, 5, 110, 80)
        $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $c1, $c2, 45.0)
        $g.FillPolygon($brush, $pts)

        $borderPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 110, 231, 183), [Math]::Max(1.5, [float]($size * 0.04)))
        $g.DrawPolygon($borderPen, $pts)

        # Lightning bolt points
        $cx = [float]($pad + $w * 0.5)
        $cy = [float]($pad + $h * 0.5)
        $lw = [float]($w * 0.35)
        $lh = [float]($h * 0.55)

        $boltPts = [System.Drawing.PointF[]]@(
            (New-Object System.Drawing.PointF(($cx + $lw * 0.15), ($cy - $lh * 0.5))),
            (New-Object System.Drawing.PointF(($cx - $lw * 0.45), ($cy + $lh * 0.05))),
            (New-Object System.Drawing.PointF(($cx - $lw * 0.05), ($cy + $lh * 0.05))),
            (New-Object System.Drawing.PointF(($cx - $lw * 0.25), ($cy + $lh * 0.5))),
            (New-Object System.Drawing.PointF(($cx + $lw * 0.45), ($cy - $lh * 0.05))),
            (New-Object System.Drawing.PointF(($cx + $lw * 0.05), ($cy - $lh * 0.05)))
        )

        $bc1 = [System.Drawing.Color]::FromArgb(255, 255, 235, 59) # Bright Yellow
        $bc2 = [System.Drawing.Color]::FromArgb(255, 245, 158, 11) # Amber
        $boltBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $bc1, $bc2, 90.0)
        $g.FillPolygon($boltBrush, $boltPts)

        $boltPen = New-Object System.Drawing.Pen([System.Drawing.Color]::White, [Math]::Max(1.0, [float]($size * 0.02)))
        $g.DrawPolygon($boltPen, $boltPts)

        $g.Dispose()

        $ms = New-Object System.IO.MemoryStream
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        $pngs += ,$ms.ToArray()
    }

    if (!(Test-Path "assets")) { New-Item -ItemType Directory "assets" -Force | Out-Null }
    $icoPath = "assets\app.ico"
    $fs = [System.IO.File]::Create($icoPath)
    $bw = New-Object System.IO.BinaryWriter($fs)

    $bw.Write([UInt16]0)
    $bw.Write([UInt16]1)
    $bw.Write([UInt16]$sizes.Count)

    $offset = 6 + ($sizes.Count * 16)
    for ($i = 0; $i -lt $sizes.Count; $i++) {
        $sz = $sizes[$i]
        $pngData = $pngs[$i]
        $bWidth = if ($sz -ge 256) { 0 } else { [byte]$sz }
        $bHeight = if ($sz -ge 256) { 0 } else { [byte]$sz }

        $bw.Write([byte]$bWidth)
        $bw.Write([byte]$bHeight)
        $bw.Write([byte]0)
        $bw.Write([byte]0)
        $bw.Write([UInt16]1)
        $bw.Write([UInt16]32)
        $bw.Write([UInt32]$pngData.Length)
        $bw.Write([UInt32]$offset)
        $offset += $pngData.Length
    }

    for ($i = 0; $i -lt $sizes.Count; $i++) {
        $bw.Write($pngs[$i])
    }

    $bw.Flush()
    $fs.Close()
    Write-Host "Created $icoPath successfully! File size: $( (Get-Item $icoPath).Length ) bytes"
}

Create-ShieldIcon

