@echo off
setlocal

if exist "%LOCALAPPDATA%\Pandoc\pandoc.exe" set "PATH=%LOCALAPPDATA%\Pandoc;%PATH%"
if exist "%LOCALAPPDATA%\Programs\MiKTeX\miktex\bin\x64\xelatex.exe" set "PATH=%LOCALAPPDATA%\Programs\MiKTeX\miktex\bin\x64;%PATH%"

where pandoc >nul 2>nul
if %errorlevel% neq 0 (
  echo Pandoc was not found on PATH. Install Pandoc and reopen this shell.
  exit /b 1
)
where xelatex >nul 2>nul
if %errorlevel% neq 0 (
  echo XeLaTeX was not found on PATH. Install MiKTeX or TeX Live and reopen this shell.
  exit /b 1
)

echo Building PDF (generating .tex then running XeLaTeX three times)...
pandoc -s docs\BOOK.md -o "docs\The-IRIS-Programming-Language.tex" ^
  --lua-filter=docs\pdf-filter.lua ^
  --syntax-definition=docs\iris.xml ^
  --syntax-highlighting=docs\iris-theme.theme ^
  --toc --toc-depth=1 ^
  --top-level-division=part -V book=true ^
  -V documentclass=book -V classoption=oneside ^
  -V "title=The IRIS Programming Language" ^
  -V "author=Moon9t" ^
  -V "date=November 2026" ^
  -V "lang=en"

if %errorlevel% neq 0 (
  echo Failed to generate LaTeX source!
  exit /b %errorlevel%
)

REM Run XeLaTeX three times to resolve cross-references, TOC pages, and bookmarks
xelatex -interaction=nonstopmode -halt-on-error -output-directory=docs docs\The-IRIS-Programming-Language.tex
if %errorlevel% neq 0 (
  echo XeLaTeX first pass failed!
  exit /b %errorlevel%
)
xelatex -interaction=nonstopmode -halt-on-error -output-directory=docs docs\The-IRIS-Programming-Language.tex
if %errorlevel% neq 0 (
  echo XeLaTeX second pass failed!
  exit /b %errorlevel%
)
xelatex -interaction=nonstopmode -halt-on-error -output-directory=docs docs\The-IRIS-Programming-Language.tex
if %errorlevel% neq 0 (
  echo XeLaTeX third pass failed!
  exit /b %errorlevel%
)

echo PDF built successfully!
