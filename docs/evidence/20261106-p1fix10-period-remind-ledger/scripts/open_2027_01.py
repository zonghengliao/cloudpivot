import os
import sys
sys.path.insert(0, '/root/r10-run')
from apilib import Api

a = Api()
a.login('admin', os.environ['R10_ADMIN_PASSWORD'])
print(a.call('POST', '/business/erp/period', {'period_year': 2027, 'period_month': 1, 'remark': 'R10 验收开账'}))
