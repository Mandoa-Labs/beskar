
```
curl -s https://api.github.com/repos/Mandoa-Labs/beskar/releases/latest \
| grep "browser_download_url.*\amd64.deb" \
| cut -d : -f 2,3 \
| tr -d \" \
| wget -qi -

```
