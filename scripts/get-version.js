const fs = require('fs')
const path = require('path')

const cargoPath = path.resolve(__dirname, '../Cargo.toml')
if (!fs.existsSync(cargoPath)) {
  console.log('0.1.0')
  process.exit(0)
}

const content = fs.readFileSync(cargoPath, 'utf8')
const match = content.match(/\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"/)
console.log(match ? match[1] : '0.1.0')
