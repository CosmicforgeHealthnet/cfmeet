/**
 * PDF Generator Script
 *
 * Run: npm install puppeteer && node docs/generate-pdf.js
 *
 * Or simply open docs/ui-ux-design-guide.html in Chrome and
 * press Ctrl+P to print to PDF with these settings:
 * - Destination: Save as PDF
 * - Layout: Portrait
 * - Paper size: Letter or A4
 * - Margins: Default
 * - Background graphics: Enabled
 */

const puppeteer = require('puppeteer');
const path = require('path');

async function generatePDF() {
    const browser = await puppeteer.launch();
    const page = await browser.newPage();

    const htmlPath = path.join(__dirname, 'ui-ux-design-guide.html');
    const pdfPath = path.join(__dirname, 'CosmicForge-RTC-UI-UX-Design-Guide.pdf');

    await page.goto(`file://${htmlPath}`, { waitUntil: 'networkidle0' });

    await page.pdf({
        path: pdfPath,
        format: 'Letter',
        printBackground: true,
        margin: {
            top: '0',
            right: '0',
            bottom: '0',
            left: '0'
        }
    });

    await browser.close();
    console.log(`PDF generated: ${pdfPath}`);
}

generatePDF().catch(console.error);
